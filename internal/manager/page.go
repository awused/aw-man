package manager

import (
	"fmt"
	"image"

	_ "image/jpeg"
	_ "image/png"

	_ "golang.org/x/image/webp"
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

	extractCh <-chan string  // buffered
	normal    *loadableImage // nullable

	upscaleCh chan string    // buffered
	upscale   *loadableImage // nullable
}

func (p *page) String() string {
	return fmt.Sprintf("[i:%s %d]", p.name, p.state)
}

// This will not cancel any ongoing loads but will discard their results.
func (p *page) unload() {
	p.normal.unload()
	p.upscale.unload()
}

func (p *page) invalidateScaledImages(sz image.Point) {
	p.normal.invalidateScaledImages(sz)
	p.upscale.invalidateScaledImages(sz)
}

// Wait until nothing is being done on the image, then delete it.
func (p *page) cleanup() {
	p.unload()
	p.normal.cleanup()
	newLoadableImage(<-p.extractCh).cleanup()
	p.upscale.cleanup()
	if p.state == upscaling {
		newLoadableImage(<-p.upscaleCh).cleanup()
	}
}
