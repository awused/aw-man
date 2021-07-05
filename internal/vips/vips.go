// +build !novips

package vips

/*
#cgo pkg-config: vips
#include "vips.h"
*/
import "C"
import (
	"bytes"
	"image"
	"image/png"
	"io/ioutil"
	"path/filepath"
	"strings"
	"sync"
	"unsafe"

	"github.com/h2non/bimg"
)

var ignoredExtensions = map[string]bool{
	".csv": true,
	".dz":  true,
	".mat": true,
	".img": true,
}

var supportedExtensions = map[string]bool{}

var once sync.Once

// IsSupportedImage returns if the image is supported by libvips,
// unless it is a format that is deliberately ignored.
func IsSupportedImage(f string) bool {
	once.Do(func() {
		sufs := C.get_suffixes()
		defer C.free_str_array(sufs)
		length := C.len_chars(sufs)

		tmpslice := (*[1 << 30]*C.char)(unsafe.Pointer(sufs))[:length:length]
		for _, cs := range tmpslice {
			s := C.GoString(cs)
			if s != "" && !ignoredExtensions[s] {
				supportedExtensions[s] = true
			}
		}
	})

	ext := strings.ToLower(filepath.Ext(f))
	return supportedExtensions[ext]
}

// ConvertImageToPNG converts the image at src to a png and writes it to dst
func ConvertImageToPNG(src, dst string) error {
	buf, err := ioutil.ReadFile(src)
	if err != nil {
		return err
	}

	pngbuf, err := bimg.NewImage(buf).Convert(bimg.PNG)
	if err != nil {
		return err
	}

	return ioutil.WriteFile(dst, pngbuf, 0644)
}

// ReadImageFromFile reads an image from the file, converts it to PNG, then converts it to a native
// Go image.
// This should only be used as a rare fallback for natively supported formats that Go's standard
// library doesn't understand. See https://github.com/golang/go/issues/10447
func ReadImageFromFile(src string) (image.Image, error) {
	buf, err := ioutil.ReadFile(src)
	if err != nil {
		return nil, err
	}

	pngbuf, err := bimg.NewImage(buf).Process(bimg.Options{Type: bimg.PNG, Compression: 1})
	if err != nil {
		return nil, err
	}

	return png.Decode(bytes.NewReader(pngbuf))
}
