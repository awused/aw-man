package manager

import (
	"bytes"
	"fmt"
	"image"
	"math"
	"os"
	"path/filepath"
	"strconv"

	// Loaded for side effects
	_ "image/jpeg"
	_ "image/png"

	"golang.org/x/image/draw"
	// Loaded for side effects
	_ "golang.org/x/image/webp"

	"github.com/awused/aw-manga/internal/closing"
	log "github.com/sirupsen/logrus"
)

var loadingSem chan struct{}

type liState int8

const (
	unwritten liState = iota
	unloaded
	// The image is not loaded at all or is not pre-scaled.
	// Image data may be present but scaled to the wrong resolution or using a
	// lower quality method.
	loading
	// The pre-scaled image data is present in memory
	loaded
	failed
)

// GetScalingMethod returns the scaling method to use.
// The normal method is a slow but higher quality image, but can be slow when
// responding to user input.
func GetScalingMethod(fast bool) draw.Scaler {
	if fast {
		return draw.ApproxBiLinear
	}
	return draw.CatmullRom
}

// Pre-scale all images if necessary for display purposes.
// They will be invalidated and reloaded if the target resolution
// changes. This costs performance and wasted work on window resizes but
// reduces memory usage and increases performance for normal viewing.
type maybeScaledImage struct {
	img            image.Image //nullable
	originalBounds image.Rectangle
	scaled         bool
	fastScale      bool // if it was scaled with a faster method to unblock the UI
}

// An image that is available on the filesystem to be loaded or upscaled.
type loadableImage struct {
	file      string
	state     liState
	deletable bool // It's deleteable if we created it

	// The current load if it has not been cancelled.
	// Buffered channel of size 1.
	loadCh chan maybeScaledImage // buffered
	// Used to track loads even if they've been cancelled.
	lastLoad     <-chan struct{}
	cancelLoadCh chan struct{}
	msi          maybeScaledImage
}

func (li *loadableImage) String() string {
	return fmt.Sprintf("[l:%s %d]", li.file, li.state)
}

// CanLoad returns true if a load can be initiated for this loadable image,
// though potentially not yet.
func (li *loadableImage) CanLoad() bool {
	return li.state <= unloaded
}

// ReadyToLoad returns true if a load can be initiated right now.
func (li *loadableImage) ReadyToLoad() bool {
	return li.state == unloaded
}

// HasImageData returns if this image can be displayed.
// It may be in the process of rescaling itself.
func (li *loadableImage) HasImageData() bool {
	return li.state == loaded || (li.state == loading && li.msi.img != nil)
}

// Delete unloads and  deletes the image, only if it's deletable
func (li *loadableImage) Delete() {
	if !li.deletable {
		// Should never happen since this is only called on upscaled images
		log.Panicln("Asked to delete file we did not create.", li)
	}

	removeFile(li.file)
}

// MarkLoaded finalizes the loading state of the image
func (li *loadableImage) MarkLoaded(msi maybeScaledImage) {
	li.msi = msi
	li.state = loaded
	if msi.img == nil {
		li.state = failed
	}
	//log.Debugln("Finished loading   ", li)
}

func (li *loadableImage) join() {
	// Wait until we're certain we don't have the image open anymore
	<-li.lastLoad
}

// Unloads a loaded image.
// Doesn't cancel an ongoing load, but does discard its results.
func (li *loadableImage) unload() {
	if li.state == failed || li.state == unwritten {
		return
	}

	if li.state == loading {
		//oldLoad := li.loadCh
		// Remake the channel so that the current load gets garbage collected.
		li.loadCh = make(chan maybeScaledImage, 1)
	}

	select {
	case <-li.lastLoad:
	default:
		// Blocking means there's an ongoing load
		close(li.cancelLoadCh)
		li.cancelLoadCh = make(chan struct{})
	}

	li.state = unloaded
	li.msi = maybeScaledImage{}
}

func (li *loadableImage) invalidateScaledImages(sz image.Point, fast bool) {
	if li.state <= unloaded || li.state == failed {
		return
	}

	if li.state == loading {
		select {
		case msi := <-li.loadCh:
			li.msi = msi
			li.state = loaded
		default:
		}
	}

	if li.state == loading {
		li.unload()
		return
	}

	if li.msi.fastScale && !fast {
		li.unload()
		return
	}

	if li.msi.img.Bounds().Dx() > sz.X || li.msi.img.Bounds().Dy() > sz.Y {
		// TODO -- Could call rescale here if the image was never scaled to avoid a load.

		// There's no sense in rescaling an image already larger than the box
		// with the cheap method.
		if !fast {
			li.unload()
		}
		//log.Debugln("Unloading image", li.msi.img.Bounds().Size(), sz)
		return
	}

	if li.msi.img.Bounds() != CalculateImageBounds(li.msi.originalBounds, sz) {
		li.unload()
		return
	}
}

// Loads the image synchronously and scales it using a cheaper method.
// Should only be done in the main thread.
// returns the original image.
func (li *loadableImage) loadSync(
	bounds image.Point, fastScale bool) image.Image {
	if li.state == unwritten {
		log.Panicln("Tried to synchronously load unwritten file.", li)
	}

	if li.state == loaded {
		return nil
	}

	log.Debugln("Synchronous load   ", li)
	if li.state == loading {
		// TODO -- do we want to jump the semaphore queue here?
		li.msi = <-li.loadCh
		li.state = loaded
		return nil
	}

	<-li.lastLoad // Do we need to care?
	li.state = loaded
	img := loadImageFromFile(li.file)
	if img != nil {
		li.msi = maybeScaleImage(img, bounds, fastScale)
	} else {
		li.state = failed
	}
	return img
}

// Rescales an image with the slower method, if necessary.
func (li *loadableImage) Rescale(bounds image.Point, img image.Image) {
	if li.state != loaded || img == nil {
		return
	}

	li.load(bounds, img)
}

// Load starts loading the image asynchronously
func (li *loadableImage) Load(bounds image.Point) {
	if li.state != unloaded {
		log.Panicln("Tried to load image that isn't ready", li)
		return
	}

	li.load(bounds, nil)
}

func (li *loadableImage) load(bounds image.Point, img image.Image) {
	//log.Debugln("Started loading    ", li)
	lastLoad := li.lastLoad
	thisLoad := make(chan struct{})
	li.lastLoad = thisLoad
	li.state = loading

	go loadAndScale(li.file, bounds, li.loadCh, li.cancelLoadCh, lastLoad, thisLoad, img)
}

func loadAndScale(
	file string,
	bounds image.Point,
	loadCh chan<- maybeScaledImage,
	cancelLoad <-chan struct{},
	lastLoad <-chan struct{},
	thisLoad chan<- struct{},
	img image.Image, // nullable
) {
	defer close(thisLoad)

	var msi maybeScaledImage

	// Always send something, even if empty
	defer func() {
		loadCh <- msi
	}()

	select {
	case <-lastLoad:
		// Blocking here represents the rare case where a file is loaded,
		// unloaded, and loaded again before the first load has finished.
	case <-closing.Ch:
		return
	case <-cancelLoad:
		return
	}

	loadingSem <- struct{}{}
	defer func() { <-loadingSem }()

	select {
	case <-closing.Ch:
		return
	case <-cancelLoad:
		log.Debugln("Load pre-empted")
		return
	default:
	}

	if img == nil {
		img = loadImageFromFile(file)
	}

	if img != nil {
		msi = maybeScaleImage(img, bounds, false)
	}
}

func loadImageFromFile(file string) image.Image {
	f, err := os.Open(file)
	if err != nil {
		log.Errorf("Error opening %s: %+v\n", file, err)
		return nil
	}
	defer f.Close()

	img, _, err := image.Decode(f)
	if err != nil {
		log.Errorf("Error decoding %s: %+v\n", file, err)
		return nil
	}

	return img
}

func maybeScaleImage(img image.Image, bounds image.Point, fastScale bool) maybeScaledImage {
	msi := maybeScaledImage{
		scaled:         false,
		fastScale:      fastScale,
		originalBounds: img.Bounds(),
	}

	newBounds := CalculateImageBounds(img.Bounds(), bounds)
	if bounds == (image.Point{}) {
		log.Infoln("Asked to scale image with no known bounds")
		// We don't have a resolution yet, should only happen on initial load
		newBounds = img.Bounds()
	}

	rgba := image.NewRGBA(newBounds)
	if newBounds == img.Bounds() {
		// No scaling, but convert to RGBA anyway
		draw.Draw(rgba, rgba.Bounds(), img, img.Bounds().Min, draw.Src)
	} else {
		msi.scaled = true
		GetScalingMethod(fastScale).Scale(
			rgba,
			rgba.Bounds(),
			img,
			img.Bounds(),
			draw.Src,
			nil)
	}

	msi.img = rgba

	return msi
}

// Assumes file will be created before this loadable image is used for
// upscaling.
func (li *loadableImage) loadFromBytes(
	buf []byte,
	bounds image.Point) error {

	img, _, err := image.Decode(bytes.NewReader(buf))
	if err != nil {
		li.state = failed
		return err
	}

	li.state = loaded
	li.msi = maybeScaleImage(img, bounds, false)
	return nil
}

// CalculateImageBounds determines the bounds for the "fit to container"
// display mode.
func CalculateImageBounds(
	img image.Rectangle, size image.Point) image.Rectangle {
	ix, iy := img.Size().X, img.Size().Y
	cx, cy := size.X, size.Y
	nx, ny := ix, iy

	if ix > cx || iy > cy {
		scale := math.Min(float64(cx)/float64(ix), float64(cy)/float64(iy))
		nx = int(scale * float64(ix))
		ny = int(scale * float64(iy))
	}

	return image.Rectangle{
		Max: image.Point{X: nx, Y: ny},
	}
}

func newExtractedImage(
	inArchivePath string, tmpDir string, n int) loadableImage {
	lastLoad := make(chan struct{})
	close(lastLoad)

	path := filepath.Join(
		tmpDir, strconv.Itoa(n)+filepath.Ext(inArchivePath))

	return loadableImage{
		file:         path,
		deletable:    true,
		state:        unwritten,
		loadCh:       make(chan maybeScaledImage, 1),
		cancelLoadCh: make(chan struct{}),
		lastLoad:     lastLoad,
	}
}

func newUpscaledImage(tmpDir string, n int) loadableImage {
	lastLoad := make(chan struct{})
	close(lastLoad)

	// png is lossless and faster to write than webp
	path := filepath.Join(tmpDir, "up"+strconv.Itoa(n)+".png")

	return loadableImage{
		file:         path,
		deletable:    true,
		state:        unwritten,
		loadCh:       make(chan maybeScaledImage, 1),
		cancelLoadCh: make(chan struct{}),
		lastLoad:     lastLoad,
	}
}

func newExistingImage(path string) loadableImage {
	lastLoad := make(chan struct{})
	close(lastLoad)

	return loadableImage{
		file:         path,
		deletable:    false,
		state:        unloaded,
		loadCh:       make(chan maybeScaledImage, 1),
		cancelLoadCh: make(chan struct{}),
		lastLoad:     lastLoad,
	}
}
