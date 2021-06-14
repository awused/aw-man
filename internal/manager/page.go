package manager

import (
	"fmt"
	"image"
	"os"
	"path/filepath"
	"strings"

	"github.com/awused/aw-manga/internal/config"
	log "github.com/sirupsen/logrus"
)

type eiState int8

const (
	extracting eiState = iota
	extracted          // Extraction has finished, success or failure
	upscaling
	upscaled // Upscaling has finished, success or failure
)

type page struct {
	// name is displayed to the user.
	// It is path with any prefix directories common to all files removed.
	name  string
	path  string
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
// Returns false is the page is already loaded, currently loading, failed
// to load, or failed to be written.
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
		p.normal.state = unloaded
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
		p.upscale.state = unloaded
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

func (p *page) invalidateScaledImages(sz image.Point) {
	p.normal.invalidateScaledImages(sz, false)
	p.upscale.invalidateScaledImages(sz, false)
}

// Wait until nothing is being done on the image, and delete the upscaled version.
// The regular version, if it was extracted, will only be cleared when the archive is unloaded.
func (p *page) cleanup() {
	p.unload()
	p.normal.join()
	<-p.extractCh

	p.clearUpscale()
	<-p.prevUpscale
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
				p.upscale.state = unloaded
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
	path string, n int, tmpDir string) *page {

	prevUp := make(chan struct{})
	close(prevUp)

	return &page{
		name:        path,
		path:        path,
		state:       extracting,
		extractCh:   make(chan bool, 1),
		upscaleCh:   make(chan bool, 1),
		prevUpscale: prevUp,
		normal:      newExtractedImage(path, tmpDir, n),
		upscale:     newUpscaledImage(tmpDir, n),
	}
}

func newDirectoryPage(
	fileName string, dir string, n int, tmpDir string) *page {

	prevUp := make(chan struct{})
	exCh := make(chan bool, 1)
	close(prevUp)
	close(exCh)

	return &page{
		name:        fileName,
		path:        fileName,
		state:       extracted, // Starts in the extracted state
		extractCh:   exCh,
		upscaleCh:   make(chan bool, 1),
		prevUpscale: prevUp,
		normal:      newExistingImage(filepath.Join(dir, fileName)),
		upscale:     newUpscaledImage(tmpDir, n),
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
