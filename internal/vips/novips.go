// +build novips

package vips

import "errors"

// IsSupportedImage returns false.
func IsSupportedImage(f string) bool {
	return false
}

// ConvertImageToPNG does nothing.
func ConvertImageToPNG(src, dst string) error {
	return errors.New("Not supported.")
}
