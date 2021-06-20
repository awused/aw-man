package manager

import (
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

	"github.com/awused/aw-man/internal/closing"
	log "github.com/sirupsen/logrus"
)

var loadingSem chan struct{}

type liState int8

const (
	unwritten liState = iota
	// Images are usually written during extraction, except in the case of file types that
	// Go does not natively support.
	// Those will be converted to a supported format at loading time.
	loadable
	// The image is not loaded at all or is not pre-scaled.
	// Image data may be present but scaled to the wrong resolution or using a
	// lower quality method.
	loading
	// The pre-scaled image data is present in memory
	loaded
	failed
)

// png is faster to write than webp.
// TODO -- this needs benchmarking, since webp gives smaller files
const extension = ".png"

// Pre-scale all images if necessary for display purposes.
// They will be invalidated and reloaded if the target resolution
// changes. This costs performance and wasted work on window resizes but
// reduces memory usage and increases performance for normal viewing.
type maybeScaledImage struct {
	img            image.Image //nullable
	originalBounds image.Rectangle
	scaled         bool
}

// An image that is available on the filesystem to be loaded or upscaled.
type loadableImage struct {
	file  string
	state liState
	// It's deleteable if we wrote it
	deletable bool

	// Non-empty if this file needs to be converted using ImageMagick from an unsupported format.
	unconvertedFile string

	// The current load if it has not been cancelled.
	// Buffered channel of size 1.
	loadCh chan maybeScaledImage // buffered
	// Used to track loads even if they've been cancelled.
	lastLoad     <-chan struct{}
	cancelLoadCh chan struct{}
	msi          maybeScaledImage
	// The target size of the most recent load, even if ongoing.
	targetSize image.Point
}

func (li *loadableImage) String() string {
	return fmt.Sprintf("[l:%s %d]", li.file, li.state)
}

// CanLoad returns true if a load can be initiated for this loadable image,
// though potentially not yet.
func (li *loadableImage) CanLoad() bool {
	return li.state <= loadable
}

// ReadyToLoad returns true if a load can be initiated right now.
func (li *loadableImage) ReadyToLoad( /*mustConvert bool*/ ) bool {
	return li.state == loadable
}

// HasImageData returns if this image can be displayed.
// It may be in the process of rescaling itself.
func (li *loadableImage) HasImageData() bool {
	return li.state == loaded || (li.state == loading && li.msi.img != nil)
}

// IsLoading returns whether the image is currently loading
func (li *loadableImage) IsLoading() bool {
	return li.state == loading
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

	li.state = loadable
	li.msi = maybeScaledImage{}
}

func (li *loadableImage) invalidateDownscaled(sz image.Point) {
	if li.state <= loadable || li.state == failed {
		return
	}

	if li.targetSize == (image.Point{}) {
		return
	}

	if li.state == loading {
		select {
		case msi := <-li.loadCh:
			li.MarkLoaded(msi)
		default:
		}
	}

	if li.state == loading && li.targetSize != sz {
		li.unload()
	}

	if li.state == loaded &&
		li.msi.img.Bounds() != CalculateImageBounds(li.msi.originalBounds, sz) {
		li.unload()
	}
}

func (li *loadableImage) maybeRescale(sz image.Point) {
	if sz == (image.Point{}) {
		return
	}

	li.invalidateDownscaled(sz)

	if li.state == loading {
		return
	}

	if li.state != loaded {
		return
	}

	if li.state == loaded &&
		li.msi.img.Bounds() == CalculateImageBounds(li.msi.originalBounds, sz) {
		return
	}

	li.rescale(sz, li.msi.img)
}

// Loads the image synchronously unless it was already being loaded normally.
// Should only be done in the main thread.
// returns the original image.
// TODO -- If the gui is always going to use the cheap method,
// this method can be simplified to just load it unscaled.
func (li *loadableImage) loadSyncUnscaled() {
	if li.state == unwritten {
		log.Panicln("Tried to synchronously load unwritten file.", li)
	}

	if li.state == loaded || li.state == loading {
		return
	}

	if li.state == loading {
		li.msi = <-li.loadCh
		li.state = loaded
		return
	}

	log.Debugln("Synchronous load   ", li)
	li.targetSize = image.Point{}
	li.state = loaded
	img := loadImageFromFile(li.file)
	if img != nil {
		li.msi = maybeScaledImage{
			img:            img,
			originalBounds: img.Bounds(),
			scaled:         false,
		}
	} else {
		li.state = failed
	}
}

// Rescales an image with the slower method, if necessary.
func (li *loadableImage) rescale(targetSize image.Point, original image.Image) {
	if li.state != loaded || original == nil {
		return
	}

	log.Debugln("Rescaling", li, original.Bounds().Max, "->", targetSize)
	li.load(targetSize, original)
}

// Load starts loading the image asynchronously
func (li *loadableImage) Load(targetSize image.Point) {
	if li.state != loadable {
		log.Panicln("Tried to load image that isn't ready", li)
		return
	}

	log.Debugln("Loading", li)
	li.load(targetSize, nil)
}

func (li *loadableImage) load(targetSize image.Point, img image.Image) {
	//log.Debugln("Started loading    ", li)
	lastLoad := li.lastLoad
	thisLoad := make(chan struct{})
	li.lastLoad = thisLoad
	li.state = loading
	li.targetSize = targetSize

	go loadAndScale(li.file, targetSize, li.loadCh, li.cancelLoadCh, lastLoad, thisLoad, img)
}

func loadAndScale(
	file string,
	targetSize image.Point,
	loadCh chan<- maybeScaledImage,
	cancelLoad <-chan struct{},
	lastLoad <-chan struct{},
	thisLoad chan<- struct{},
	img image.Image, // nullable
) {
	defer func() {
		<-lastLoad
		close(thisLoad)
	}()

	var msi maybeScaledImage

	// Always send something, even if empty
	defer func() {
		loadCh <- msi
	}()

	select {
	case <-closing.Ch:
		return
	case <-cancelLoad:
		log.Debugln("Load pre-empted")
		return
	default:
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

	select {
	case <-closing.Ch:
		return
	case <-cancelLoad:
		log.Debugln("Load pre-empted")
		return
	default:
	}

	if img != nil {
		msi = maybeScaleImage(img, targetSize)
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

func maybeScaleImage(img image.Image, targetSize image.Point) maybeScaledImage {
	msi := maybeScaledImage{
		scaled:         false,
		originalBounds: img.Bounds(),
	}

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
		msi.scaled = true
		draw.CatmullRom.Scale(
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

func newExtractedImage(file string) loadableImage {
	lastLoad := make(chan struct{})
	close(lastLoad)

	return loadableImage{
		file:         file,
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
		state:        loadable,
		loadCh:       make(chan maybeScaledImage, 1),
		cancelLoadCh: make(chan struct{}),
		lastLoad:     lastLoad,
	}
}

// Used when
func newConvertedImage(tmpDir string, n int, originalFile string) loadableImage {
	lastLoad := make(chan struct{})
	close(lastLoad)

	// png is lossless and faster to write than webp
	path := filepath.Join(tmpDir, strconv.Itoa(n)+".png")

	return loadableImage{
		file:            path,
		deletable:       true,
		state:           unwritten,
		unconvertedFile: originalFile,
		loadCh:          make(chan maybeScaledImage, 1),
		cancelLoadCh:    make(chan struct{}),
		lastLoad:        lastLoad,
	}
}
