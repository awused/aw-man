package manager

import (
	"bytes"
	"fmt"
	"image"
	"math"
	"os"

	_ "image/jpeg"
	_ "image/png"

	"golang.org/x/image/draw"
	_ "golang.org/x/image/webp"

	"github.com/awused/aw-manga/internal/closing"
	log "github.com/sirupsen/logrus"
)

var loadingSem chan struct{}

type liState int8

const (
	unloaded liState = iota
	loading
	loaded // Loading is finished, even if it failed
)

var ScalingMethod = draw.CatmullRom

// Pre-scale all images if necessary for display purposes.
// But they will need to be invalidated and reloaded if the target resolution
// changes.
type maybeScaledImage struct {
	img    image.Image //nullable
	scaled bool
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

func (li *loadableImage) cleanup() {
	if li == nil {
		return
	}

	// Wait until we're certain we don't have the image open anymore
	<-li.lastLoad

	os.Remove(li.file)
}

// Unloads a loaded image.
// Doesn't cancel an ongoing load, but does discard its results.
func (li *loadableImage) unload() {
	if li == nil {
		return
	}

	if li.state == loading {
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

func (li *loadableImage) invalidateScaledImages(sz image.Point) {
	if li == nil || li.state == unloaded {
		return
	}

	if li.state == loading {
		select {
		case msi := <-li.loadCh:
			li.msi = msi
			li.state = loaded
			log.Debugln("Finished loading   ", li)
		default:
		}
	}

	if li.state == loading {
		li.unload()
		return
	}

	if li.msi.img != nil &&
		(li.msi.img.Bounds().Dx() > sz.X || li.msi.img.Bounds().Dy() > sz.Y) {
		log.Debugln("Unloading image", li.msi.img.Bounds().Size(), sz)
		li.unload()
	}

	if li.msi.scaled &&
		(li.msi.img.Bounds().Dx() == sz.X || li.msi.img.Bounds().Dy() == sz.Y) {
		// Keep it if we would end up with the same size after a reload.
		return
	}
	li.unload()
}

func (li *loadableImage) load(targetSize image.Point) {
	if li.state != unloaded {
		return
	}

	log.Debugln("Started loading    ", li)
	lastLoad := li.lastLoad
	thisLoad := make(chan struct{})
	li.lastLoad = thisLoad
	li.state = loading

	go loadFile(li.file, targetSize, li.loadCh, li.cancelLoadCh, lastLoad, thisLoad)
}

// Loads synchronously if there is no ongoing async load already.
// TODO -- maybe this for resizing performance?
//func (li *loadableImage) loadSync(targetSize image.Point) {
//}

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
	targetSize image.Point,
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
		return
	default:
	}

	f, err := os.Open(file)
	if err != nil {
		log.Errorf("Error opening %s: %+v\n", file, err)
		return
	}
	defer f.Close()

	img, _, err := image.Decode(f)
	if err != nil {
		log.Errorf("Error decoding %s: %+v\n", file, err)
		return
	}

	msi = maybeScaleImage(img, targetSize)
}

func maybeScaleImage(img image.Image, targetSize image.Point) maybeScaledImage {
	scaled := false
	newBounds := CalculateImageBounds(img.Bounds(), targetSize)
	if targetSize == (image.Point{}) {
		log.Infoln("Asked to scale image with no known bounds")
		// We don't have a resolution yet, should only happen on initial load
		newBounds = img.Bounds()
	}

	rgba := image.NewRGBA(newBounds)
	if newBounds == img.Bounds() {
		// No scaling, but convert to RGBA anyway
		draw.Draw(rgba, rgba.Bounds(), img, img.Bounds().Min, draw.Src)
	} else {
		scaled = true
		ScalingMethod.Scale(
			rgba,
			rgba.Bounds(),
			img,
			img.Bounds(),
			draw.Src,
			nil)
	}

	return maybeScaledImage{
		img:    rgba,
		scaled: scaled,
	}
}

// Assumes file will be created before this loadable image is used for
// upscaling.
func loadableFromBytes(
	file string,
	targetSize image.Point,
	buf []byte) *loadableImage {

	img, _, err := image.Decode(bytes.NewReader(buf))
	if err != nil {
		return nil
	}

	lastLoad := make(chan struct{})
	close(lastLoad)
	return &loadableImage{
		file:         file,
		msi:          maybeScaleImage(img, targetSize),
		state:        loaded,
		loadCh:       make(chan maybeScaledImage, 1),
		cancelLoadCh: make(chan struct{}),
		lastLoad:     lastLoad,
	}
}

func CalculateImageBounds(img image.Rectangle, size image.Point) image.Rectangle {
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
