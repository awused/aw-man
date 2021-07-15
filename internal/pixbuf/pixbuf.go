package pixbuf

import (
	"path/filepath"
	"runtime"
	"strings"
	"sync"

	"github.com/gotk3/gotk3/gdk"
)

var ignoredExtensions = map[string]bool{
	// Something screwy happens when handling jxl and avif/heif images, even if using glib.AddIdle()
	// to perform everything in the main thread..
	// Haven't root caused it, but disabling jxl or avif/heif seems to avoid it even in mixed folders
	// and I don't have any real avif/heif images.
	// NOTE -- this crash reproduces in Mcomix3 as well.
	// TODO -- figure this out.
	".avif": true,
	".heif": true,
}

var supportedExtensions = map[string]bool{}

var once sync.Once

// IsSupportedImage returns if the image is supported by gdk,
// unless it is a format that is deliberately ignored.
func IsSupportedImage(f string) bool {
	once.Do(func() {
		for _, v := range gdk.PixbufGetFormats() {
			for _, e := range v.GetExtensions() {
				e = "." + e
				if !ignoredExtensions[e] {
					supportedExtensions[e] = true
				}
			}
		}
	})

	ext := strings.ToLower(filepath.Ext(f))
	return supportedExtensions[ext]
}

// ConvertImageToPNG converts the image at src to a png and writes it to dst
func ConvertImageToPNG(src, dst string) error {
	pb, err := gdk.PixbufNewFromFile(src)
	if err != nil {
		return err
	}
	defer runtime.KeepAlive(pb)

	return pb.SavePNG(dst, 5)
}
