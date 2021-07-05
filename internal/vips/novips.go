// +build novips

package vips

import (
	"errors"
	"image"
)

// IsSupportedImage returns false.
func IsSupportedImage(f string) bool {
	return false
}

// ConvertImageToPNG does nothing.
func ConvertImageToPNG(src, dst string) error {
	return errors.New("Not supported")
}

// ReadImageFromFile does nothing.
func ReadImageFromFile(src string) (image.Image, error) {
	return nil, errors.New("Not supported")
}
