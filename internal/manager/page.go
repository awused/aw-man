package manager

import (
	"fmt"
	"image"
	"os"
	"path/filepath"
	"strconv"
	"strings"

	"github.com/awused/aw-man/internal/config"
	log "github.com/sirupsen/logrus"
)

type eiState int8

const (
	extracting eiState = iota
	// Extraction has finished, success or failure.
	extracted
	upscaling
	// Upscaling has finished, success or failure
	upscaled
)

type page struct {
	// name is displayed to the user.
	// It is path with any prefix directories common to all files removed.
	name          string
	inArchivePath string
	// It's deletable if we created it.
	deletable bool
	// The path to the extracted file.
	// For image types that Go natively supports this will be the same as normal.file
	// For directories this will be the archivePath joined with inArchivePath.
	file  string
	state eiState

	extractCh chan bool // buffered
	normal    loadableImage

	upscaleCh chan bool // buffered
	upscale   loadableImage
	// Closed when the previous upscale is completely settled and cleaned up
	prevUpscale chan struct{}
}

func (p *page) String() string {
	return fmt.Sprintf("[i:%s %d]", p.name, p.state)
}

// Get returns the appropriate loadableImage, which may be nil
func (p *page) Get(upscaling bool) *loadableImage {
	if upscaling {
		return &p.upscale
	}
	return &p.normal
}

// CanLoad returns if the page can load, and secondarily if it would need to be
// upscaled to do so.
// Returns false if the page is already loaded, currently loading, has failed
// to load, or has failed to be written.
func (p *page) CanLoad(upscaling bool) (bool, bool) {
	if p.state == extracting {
		return true, upscaling
	}

	li := p.Get(upscaling)
	if p.state == extracted {
		return li.CanLoad(), upscaling
	}

	return li.CanLoad(), false
}

// CanUpscale  returns if the page can be upscaled.
// Returns false if upscaling has already been initiated or if extraction failed.
func (p *page) CanUpscale() bool {
	// TODO
	return false
}

// MarkExtracted finalizes the extraction state of this page based on whether
// it succeeded or not.
func (p *page) MarkExtracted(success bool) {
	if success {
		p.state = extracted
		// TODO -- only if supported
		p.normal.state = loadable
	} else {
		// There's nothing more we can do here
		p.state = upscaled
		p.normal.state = failed
		p.upscale.state = failed
	}
}

// MarkUpscaled finalizes the upscaling of this page based on whether it
// succeeded or not.
func (p *page) MarkUpscaled(success bool) {
	if success {
		p.state = upscaling
		p.upscale.state = loadable
	} else {
		// There's nothing more we can do here, but the normal image should still
		// work.
		p.state = upscaled
		p.upscale.state = failed
	}
	log.Debugln("Finished upscaling ", p)
}

// This will not cancel any ongoing loads but will discard their results.
func (p *page) unload() {
	p.normal.unload()
	p.upscale.unload()
}

func (p *page) invalidateDownscaled(sz image.Point) {
	p.normal.invalidateDownscaled(sz)
	p.upscale.invalidateDownscaled(sz)
}

func (p *page) maybeRescale(sz image.Point) {
	p.normal.maybeRescale(sz)
	p.upscale.maybeRescale(sz)
}

// Wait until nothing is being done on the image, and delete the upscaled version.
// The regular version, if it was extracted, will only be cleared when the archive is unloaded.
func (p *page) cleanup() {
	<-p.extractCh
	p.unload()
	p.normal.join()

	p.clearUpscale()
	<-p.prevUpscale

	if p.deletable && p.file != p.normal.file {
		removeFile(p.file)
	}
}

func (p *page) clearUpscale() {
	if p.state < upscaling {
		return
	}

	if p.state == upscaling {
		// Replace upscaleCh so we can never have two readers
		oldUp := p.upscaleCh
		p.upscaleCh = make(chan bool, 1)

		// Don't need to wait on the old previous upscale since we never upscale
		// the same image twice at once
		pu := make(chan struct{})
		p.prevUpscale = pu

		go func() {
			defer close(pu)
			b := <-oldUp
			if b {
				p.upscale.state = loadable
				p.normal.Delete()
			}
		}()
	}

	if p.state == upscaled {
		// Replace upscaleCh because it is closed
		p.upscaleCh = make(chan bool, 1)

		// Don't need to wait on the old previous upscale since we never upscale
		// the same image twice at once
		pu := make(chan struct{})
		p.prevUpscale = pu

		go func() {
			defer close(pu)
			p.upscale.Delete()
		}()
	}

	p.state = extracted
}

func newArchivePage(
	inArchivePath string, n int, tmpDir string) *page {

	prevUp := make(chan struct{})
	close(prevUp)

	file := filepath.Join(
		tmpDir, strconv.Itoa(n)+filepath.Ext(inArchivePath))
	var normal loadableImage
	if isNativelySupportedImage(file) {
		normal = newExtractedImage(file)
	} else {
		normal = newConvertedImage(tmpDir, n, file)
	}

	return &page{
		name:          inArchivePath,
		inArchivePath: inArchivePath,
		deletable:     true,
		file:          file,
		state:         extracting,
		extractCh:     make(chan bool, 1),
		upscaleCh:     make(chan bool, 1),
		prevUpscale:   prevUp,
		normal:        normal,
		upscale:       newUpscaledImage(tmpDir, n),
	}
}

func newDirectoryPage(
	fileName string, dir string, n int, tmpDir string) *page {

	prevUp := make(chan struct{})
	exCh := make(chan bool, 1)
	close(prevUp)
	close(exCh)

	file := filepath.Join(dir, fileName)
	var normal loadableImage
	if isNativelySupportedImage(file) {
		normal = newExistingImage(file)
	} else {
		normal = newConvertedImage(tmpDir, n, file)
	}

	return &page{
		name:          fileName,
		inArchivePath: fileName,
		deletable:     false,
		file:          file,
		state:         extracted, // Starts in the extracted state
		extractCh:     exCh,
		upscaleCh:     make(chan bool, 1),
		prevUpscale:   prevUp,
		normal:        normal,
		upscale:       newUpscaledImage(tmpDir, n),
	}
}

func removeFile(f string) {
	a, err := filepath.Abs(f)
	if err != nil {
		log.Errorln("Couldn't get absolute version of path for deletion", f, err)
		return
	}

	if !strings.HasPrefix(a, config.Conf.TempDirectory) ||
		!os.IsPathSeparator(strings.TrimPrefix(a, config.Conf.TempDirectory)[0]) {
		// Should absolutely never happen
		log.Panicln("Tried to remove file outside of the temporary directory", a)
	}

	os.Remove(a)
}
