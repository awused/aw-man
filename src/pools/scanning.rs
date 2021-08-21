use std::fs::{self, File};
use std::io::Write;
use std::path::PathBuf;
use std::sync::Arc;

use derive_more::From;
use futures_util::FutureExt;
use image::gif::GifDecoder;
use image::{AnimationDecoder, DynamicImage, ImageFormat};
use jpegxl_rs::image::ToDynamic;
use once_cell::sync::Lazy;
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::sync::{oneshot, Semaphore};

use crate::com::{Bgra, Res};
use crate::config::{CONFIG, MINIMUM_RES, TARGET_RES};
use crate::manager::files::{
    is_gif, is_jxl, is_natively_supported_image, is_pixbuf_extension, is_video_extension, is_webp,
};
use crate::pools::handle_panic;
use crate::{closing, Fut, Result};

static SCANNING_SEM: Lazy<Arc<Semaphore>> =
    Lazy::new(|| Arc::new(Semaphore::new(CONFIG.scanning_threads)));

static SCANNING: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("scan-{}", u))
        .panic_handler(handle_panic)
        .num_threads(CONFIG.scanning_threads)
        .build()
        .expect("Error creating scanning threadpool")
});

#[derive(Debug, From)]
pub enum BgraOrRes {
    Bgra(Bgra),
    Res(Res),
}

impl BgraOrRes {
    pub const fn res(&self) -> Res {
        match self {
            BgraOrRes::Bgra(bgra) => bgra.res,
            BgraOrRes::Res(r) => *r,
        }
    }

    pub fn should_upscale(&self) -> bool {
        if TARGET_RES.is_zero() {
            return false;
        }

        let r = self.res();
        ((r.w < TARGET_RES.w || TARGET_RES.w == 0) && (r.h < TARGET_RES.h || TARGET_RES.h == 0))
            || r.w < MINIMUM_RES.w
            || r.h < MINIMUM_RES.h
    }
}


#[derive(Debug)]
pub enum ScanResult {
    // If we needed to convert it to a format the image crate understands.
    // This file will be written when this result is returned, and is owned by the newly created
    // ScannedPage.
    // If the Bgra is None it means the page was unloaded while scanning, so it needs to be read
    // from scratch.
    ConvertedImage(PathBuf, BgraOrRes),
    Image(BgraOrRes),
    // Animations skip the fast path, at least for now.
    Animation,
    Video,
    Invalid(String),
}

// This is so we can consume and drop an image we don't need if the page is unloaded while
// scanning.
pub struct ScanFuture(pub Fut<ScanResult>);

impl ScanFuture {
    // In theory this can be called twice before scanning completes, but in practice it's not worth
    // optimizing for.
    pub fn unload_scanning(&mut self) {
        use BgraOrRes as BOR;
        use ScanResult::*;

        let fut = std::mem::replace(
            &mut self.0,
            // This future should never, ever be waited on. It would mean we panicked between now
            // and the end of the function.
            async { unreachable!("Waited on an invalid ScanFuture") }.boxed(),
        );
        // The entire Manager runs inside a single LocalSet so this will not panic.
        let h = tokio::task::spawn_local(async move {
            match fut.await {
                ConvertedImage(pb, BOR::Bgra(bgra)) => ConvertedImage(pb, bgra.res.into()),
                Image(BOR::Bgra(bgra)) => Image(bgra.res.into()),
                x => x,
            }
        });

        self.0 = h
            .map(|r| match r {
                Ok(r) => r,
                Err(e) => {
                    let e = format!("Unexpected error while unloading scanning page: {:?}", e);
                    error!("{}", e);
                    Invalid(e)
                }
            })
            .boxed()
    }
}

pub async fn scan(path: PathBuf, conv: PathBuf, load: bool) -> ScanFuture {
    let permit = SCANNING_SEM
        .clone()
        .acquire_owned()
        .await
        .expect("Error acquiring scanning permit");


    let (s, r) = oneshot::channel();
    SCANNING.spawn_fifo(move || {
        let result = scan_file(path, conv, load);
        let result = match result {
            Ok(sr) => sr,
            Err(e) => {
                let e = format!("Error scanning file {:?}", e);
                error!("{}", e);
                ScanResult::Invalid(e)
            }
        };

        if let Err(e) = s.send(result) {
            error!("Unexpected error scanning file {:?}", e);
        };
        drop(permit)
    });

    ScanFuture(Box::pin(async move {
        match r.await {
            Ok(sr) => sr,
            Err(e) => {
                let e = format!("Error scanning file {:?}", e);
                error!("{}", e);
                ScanResult::Invalid(e)
            }
        }
    }))
}

fn scan_file(path: PathBuf, conv: PathBuf, load: bool) -> Result<ScanResult> {
    use ScanResult::*;

    if is_gif(&path) {
        let f = File::open(&path)?;
        let decoder = GifDecoder::new(f)?;
        let mut frames = decoder.into_frames();

        let first_frame = frames.next();
        let second_frame = frames.next();

        if second_frame.is_none() && first_frame.is_some() {
            let first_frame = first_frame.unwrap()?;
            if load {
                let img = DynamicImage::ImageRgba8(first_frame.into_buffer());
                return Ok(Image(Bgra::from(img).into()));
            }
            return Ok(Image(
                Res::from((first_frame.buffer().width(), first_frame.buffer().height())).into(),
            ));
        } else if second_frame.is_some() {
            return Ok(Animation);
        }
    }

    // TODO -- animated PNGs, maybe. They're very rare in practice.

    if is_natively_supported_image(&path) {
        let img = image::open(&path);
        // Fall through to pixbuf in case this image won't load.
        // This is relevant for PNGs with invalid CRCs that pixbuf is tolerant of.
        match img {
            Ok(img) => {
                if load {
                    return Ok(Image(Bgra::from(img).into()));
                }
                return Ok(Image(Res::from(img).into()));
            }
            Err(e) => error!(
                "Error {:?} while trying to read {:?}, trying again with pixbuf.",
                e, path
            ),
        }
    }

    if is_jxl(&path) {
        let data = fs::read(&path)?;

        // TODO -- allow fall-through once the pixbuf loader is fixed?
        let decoder = jpegxl_rs::decoder_builder().build()?;
        let img = decoder
            .decode(&data)?
            .into_dynamic_image()
            .ok_or("Failed to convert jpeg-xl to DynamicImage")?;
        if load {
            return Ok(Image(Bgra::from(img).into()));
        }
        return Ok(Image(Res::from(img).into()));
    }

    if is_webp(&path) {
        let data = fs::read(&path)?;

        let features = webp::BitstreamFeatures::new(&data).ok_or("Could not read webp.")?;
        if features.has_animation() {
            return Ok(Animation);
        } else if load {
            let decoded = webp::Decoder::new(&data)
                .decode()
                .ok_or("Could not decode webp")?;
            return Ok(Image(Bgra::from(decoded.to_image()).into()));
        }
        return Ok(Image(
            Res::from((features.width(), features.height())).into(),
        ));
    }

    // TODO -- if it is possibly animated, sniff it.
    // WARNING -- PixbufAnimation is glacially slow with static webps, and sometimes fails.
    // TODO -- pixbuf loaders often leak or segfault, consider doing this in another process.

    if is_pixbuf_extension(&path) {
        let pb = gtk::gdk_pixbuf::Pixbuf::from_file(&path)?;
        let pngvec = pb.save_to_bufferv("png", &[("compression", "1")])?;

        if closing::closed() {
            return Ok(Invalid("closed".to_string()));
        }

        let mut f = File::create(&conv)?;
        f.write_all(&pngvec)?;
        drop(f);

        debug!("Converted {:?} to {:?}", path, conv);

        if !load {
            return Ok(ConvertedImage(
                conv,
                Res::from((pb.width(), pb.height())).into(),
            ));
        }

        if closing::closed() {
            return Ok(Invalid("closed".to_string()));
        }

        let img = image::load_from_memory_with_format(&pngvec, ImageFormat::Png)?;
        return Ok(ConvertedImage(conv, Bgra::from(img).into()));
    }


    if is_video_extension(&path) {
        return Ok(Video);
    }

    Ok(ScanResult::Invalid("not yet implemented".to_string()))
}
