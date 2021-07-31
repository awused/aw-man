use std::fs::File;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use futures_util::FutureExt;
use image::ImageFormat;
use once_cell::sync::Lazy;
use rayon::{ThreadPool, ThreadPoolBuilder};
use tokio::sync::{oneshot, Semaphore};

use crate::com::{Bgra, Res};
use crate::config::{CONFIG, TARGET_RES};
use crate::manager::files::{is_natively_supported_image, is_pixbuf_extension, is_webp};
use crate::{closing, Fut, Result};

static SCANNING_SEM: Lazy<Arc<Semaphore>> =
    Lazy::new(|| Arc::new(Semaphore::new(CONFIG.scanning_threads)));

static SCANNING: Lazy<ThreadPool> = Lazy::new(|| {
    ThreadPoolBuilder::new()
        .thread_name(|u| format!("scan-{}", u))
        .num_threads(CONFIG.scanning_threads)
        .build()
        .expect("Error creating scanning threadpool")
});

#[derive(Debug)]
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
        (r.w < TARGET_RES.w || TARGET_RES.w == 0) && (r.h < TARGET_RES.h || TARGET_RES.h == 0)
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
    // Animated,
    // Video,
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
                ConvertedImage(pb, BOR::Bgra(bgra)) => ConvertedImage(pb, BOR::Res(bgra.res)),
                Image(BOR::Bgra(bgra)) => Image(BOR::Res(bgra.res)),
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
                let e = format!("Error scanning image {:?}", e);
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
                let e = format!("Error scanning image {:?}", e);
                error!("{}", e);
                ScanResult::Invalid(e)
            }
        }
    }))
}

fn scan_file(path: PathBuf, conv: PathBuf, load: bool) -> Result<ScanResult> {
    use BgraOrRes as BOR;
    use ScanResult::*;

    if is_natively_supported_image(&path) {
        let img = image::open(&path)?;
        return Ok(Image(BOR::Bgra(img.into())));
    }

    if is_webp(&path) {
        let mut f = File::open(&path)?;
        let mut data = Vec::with_capacity(f.metadata()?.len() as usize + 1);
        f.read_to_end(&mut data)?;
        drop(f);

        let features = webp::BitstreamFeatures::new(&data).ok_or("Could not read webp.")?;
        if features.has_animation() {
            // TODO -- animation
            warn!("TODO -- webp animation");
        } else if load {
            let decoded = webp::Decoder::new(&data)
                .decode()
                .ok_or("Could not decode webp")?;
            return Ok(Image(BOR::Bgra(decoded.to_image().into())));
        } else {
            return Ok(Image(BOR::Res(
                (features.width(), features.height()).into(),
            )));
        }
    }

    // TODO -- if it is possibly animated, sniff it.
    // WARNING -- PixbufAnimation is glacially slow with static webps, and sometimes fails.
    // TODO -- pixbuf loaders often leak or segfault, consider doing this in another process.

    if is_pixbuf_extension(&path) {
        let pb = gtk::gdk_pixbuf::Pixbuf::from_file(&path)?;
        let pngvec = pb.save_to_bufferv("png", &[("compression", "1")])?;

        let mut f = File::create(&conv)?;
        f.write_all(&pngvec)?;
        drop(f);

        if !load {
            return Ok(ConvertedImage(
                conv,
                BOR::Res((pb.width(), pb.height()).into()),
            ));
        }

        debug!("Converted {:?} to {:?}", path, conv);
        if closing::closed() {
            return Ok(Invalid("closed".to_string()));
        }

        let img = image::load_from_memory_with_format(&pngvec, ImageFormat::Png)?;
        return Ok(ConvertedImage(conv, BOR::Bgra(img.into())));
    }


    // if is_pixbuf_extension(&path) {
    //     let pba = gtk::gdk_pixbuf::PixbufAnimation::from_file(&path)?;
    //     info!("{:?} {:?}", path, start.elapsed());
    //
    //     if pba.is_static_image() {
    //         let pb = pba.static_image().ok_or("Static pixbuf was not static.")?;
    //         info!("{:?} {:?}", path, start.elapsed());
    //         info!("{:?} {:?}", path, start.elapsed());
    //
    //         let mut f = File::create(&conv)?;
    //         f.write_all(&pngvec)?;
    //         info!("{:?} {:?}", path, start.elapsed());
    //         drop(f);
    //         debug!("Converted {:?} to {:?}", path, conv);
    //         if closing::closed() {
    //             return Ok(ScanResult::Invalid("closed".to_string()));
    //         }
    //
    //         let img = image::load_from_memory_with_format(&pngvec, ImageFormat::Png)?;
    //         return Ok(ScanResult::ConvertedImage(
    //             conv,
    //             BgraOrRes::Bgra(img.into()),
    //         ));
    //     }
    // }
    Ok(ScanResult::Invalid("not yet implemented".to_string()))
}
