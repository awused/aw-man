package manager

import (
	"bytes"
	"fmt"
	"image"
	"math"
	"os"

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
	loading
	loaded // Loading is finished, even if it failed
)

// ScalingMethod is the single scaling method used globally.
// CatmullRom is slow but
var ScalingMethod draw.Scaler = draw.CatmullRom

// Pre-scale all images if necessary for display purposes.
// They will be invalidated and reloaded if the target resolution
// changes. This costs performance and wasted work on window resizes but
// reduces memory usage and increases performance for normal viewing.
type maybeScaledImage struct {
	img       image.Image //nullable
	scaled    bool
	fastScale bool // if it was scaled with a faster method to unblock the UI
}

// An image that is available on the filesystem to be loaded or upscaled.
type loadableImage struct {
	file  string
	state liState

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
	return li == nil || li.state == unloaded
}

func (li *loadableImage) join() {
	if li == nil {
		return
	}

	// Wait until we're certain we don't have the image open anymore
	<-li.lastLoad
}

// Unloads a loaded image.
// Doesn't cancel an ongoing load, but does discard its results.
func (li *loadableImage) unload() {
	if li == nil {
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
	if li == nil || li.state == unloaded {
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

	if li.msi.img == nil {
		// Image is broken, invalidating won't help
		return
	}

	if li.msi.img.Bounds().Dx() > sz.X || li.msi.img.Bounds().Dy() > sz.Y {
		if fast {
			// There's no sense in rescaling an image already larger than the box
			// with the cheap method.
			return
		}
		//log.Debugln("Unloading image", li.msi.img.Bounds().Size(), sz)
		li.unload()
	}

	if li.msi.scaled && (!li.msi.fastScale || fast) &&
		(li.msi.img.Bounds().Dx() == sz.X || li.msi.img.Bounds().Dy() == sz.Y) {
		// Keep it if we would end up with the same image after a rescale.
		return
	}
	li.unload()
}

func (li *loadableImage) load(bounds image.Point) {
	if li.state != unloaded {
		return
	}

	log.Debugln("Started loading    ", li)
	lastLoad := li.lastLoad
	thisLoad := make(chan struct{})
	li.lastLoad = thisLoad
	li.state = loading

	go loadFile(li.file, bounds, li.loadCh, li.cancelLoadCh, lastLoad, thisLoad)
}

// Loads the image synchronously and scales it using a cheaper method.
// Should only be done in the main thread.
func (li *loadableImage) loadSync(bounds image.Point, fastScale bool) {
	if li.state == loaded {
		return
	}

	log.Debugln("Synchronous loading", li)
	if li.state == loading {
		// TODO -- do we want to jump the semaphore queue here?
		li.msi = <-li.loadCh
		li.state = loaded
		return
	}

	<-li.lastLoad // Do we need to care?
	li.state = loaded
	img := loadImageFromFile(li.file)
	if img != nil {
		li.msi = maybeScaleImage(img, bounds, fastScale)
	}
}

func newLoadableImage(f string) *loadableImage {
	if f == "" {
		return nil
	}

	lastLoad := make(chan struct{})
	close(lastLoad)
	return &loadableImage{
		file:         f,
		state:        unloaded,
		loadCh:       make(chan maybeScaledImage, 1),
		cancelLoadCh: make(chan struct{}),
		lastLoad:     lastLoad,
	}
}

func loadFile(
	file string,
	bounds image.Point,
	loadCh chan<- maybeScaledImage,
	cancelLoad <-chan struct{},
	lastLoad <-chan struct{},
	thisLoad chan<- struct{}) {
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

	img := loadImageFromFile(file)
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
		scaled:    false,
		fastScale: fastScale,
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
		scaler := ScalingMethod
		if fastScale {
			scaler = draw.ApproxBiLinear
		}
		scaler.Scale(
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
func loadableFromBytes(
	file string,
	bounds image.Point,
	buf []byte) *loadableImage {

	img, _, err := image.Decode(bytes.NewReader(buf))
	if err != nil {
		return nil
	}

	lastLoad := make(chan struct{})
	close(lastLoad)
	return &loadableImage{
		file:         file,
		msi:          maybeScaleImage(img, bounds, false),
		state:        loaded,
		loadCh:       make(chan maybeScaledImage, 1),
		cancelLoadCh: make(chan struct{}),
		lastLoad:     lastLoad,
	}
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
		//Min: image.Point{X: dx, Y: dy},
		//Max: image.Point{X: nx + dx, Y: ny + dy},
		Max: image.Point{X: nx, Y: ny},
	}
}
