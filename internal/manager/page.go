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
	name        string
	archivePath string
	state       eiState

	extractCh chan string    // buffered
	normal    *loadableImage // nullable

	upscaleCh chan string    // buffered
	upscale   *loadableImage // nullable
	// Closed when the previous upscale is completely settled and cleaned up
	prevUpscale chan struct{}

	// The page number in the archive, used to build filenames so that the
	// extracted files match the expected ordering. Tracking this as part of the
	// page makes some things slightly easier.
	number int
}

func (p *page) String() string {
	return fmt.Sprintf("[i:%s %d]", p.name, p.state)
}

// Get returns the appropriate loadableImage, which may be nil
func (p *page) Get(upscaling bool) *loadableImage {
	if upscaling {
		return p.upscale
	}
	return p.normal
}

// Returns if the page can load, and secondarily if it would need to be
// upscaled to do so.
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

func (p *page) CanUpscale() {
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
		p.upscaleCh = make(chan string, 1)

		// Don't need to wait on the old previous upscale since we never upscale
		// the same image twice at once
		pu := make(chan struct{})
		p.prevUpscale = pu

		go func() {
			defer close(pu)
			f := <-oldUp
			if f != "" {
				removeFile(f)
			}
		}()
	}

	if p.state == upscaled {
		u := p.upscale
		p.upscale = nil
		// Replace upscaleCh because it is closed
		p.upscaleCh = make(chan string, 1)

		// Don't need to wait on the old previous upscale since we never upscale
		// the same image twice at once
		pu := make(chan struct{})
		p.prevUpscale = pu

		go func() {
			defer close(pu)
			u.join()
			if u.file != "" {
				removeFile(u.file)
			}
		}()
	}

	p.state = extracted
}

func newPage(archivePath string) *page {
	path := filepath.Clean(archivePath)
	prevUp := make(chan struct{})
	close(prevUp)

	return &page{
		name:        path,
		archivePath: path,
		state:       extracting,
		extractCh:   make(chan string, 1),
		upscaleCh:   make(chan string, 1),
		prevUpscale: prevUp,
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
