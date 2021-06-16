package manager

import (
	"archive/zip"
	"io"
	"io/ioutil"
	"os"
	"path/filepath"

	"github.com/awused/aw-man/internal/closing"
	"github.com/mholt/archiver/v3"
	"github.com/nwaples/rardecode"
	log "github.com/sirupsen/logrus"
)

func archiverDiscovery(paths *[]string) archiver.WalkFunc {
	return func(f archiver.File) error {
		select {
		case <-closing.Ch:
			return archiver.ErrStopWalk
		default:
		}

		if f.IsDir() || !isNativelySupportedImage(f.Name()) {
			return nil
		}

		p := filePath(f)
		*paths = append(*paths, p)

		return nil
	}
}

func archiverExtractor(
	a *archive,
	extractionMap map[string]*page) archiver.WalkFunc {

	return func(f archiver.File) error {
		select {
		case <-closing.Ch:
			return archiver.ErrStopWalk
		case <-a.closed:
			return archiver.ErrStopWalk
		default:
		}
		success := false

		path := filePath(f)

		p, ok := extractionMap[path]
		if !ok {
			return nil
		}
		defer func() {
			// We must send to the channel after the file has closed
			p.extractCh <- success
			close(p.extractCh)
		}()
		delete(extractionMap, path)

		outF, err := os.Create(p.normal.file)
		if err != nil {
			log.Errorln("Error creating output file", a, path, p.normal, err)
			return nil
		}
		defer func() {
			if outF.Close() != nil {
				success = false
			}
		}()

		_, err = io.Copy(outF, f.ReadCloser)
		if err != nil {
			log.Errorln("Error extracting file", a, path, p.normal, err)
			return nil
		}

		success = true
		//log.Debugln("Finished extracting", p)

		return nil
	}
}

func archiverByteFetcher(page *page, buf *[]byte) archiver.WalkFunc {
	return func(f archiver.File) error {
		select {
		case <-closing.Ch:
			return archiver.ErrStopWalk
		default:
		}

		path := filepath.Clean(filePath(f))
		if path != page.inArchivePath {
			return nil
		}

		b, err := ioutil.ReadAll(f.ReadCloser)
		if err == nil {
			// Just ignore errors, we'll deal with them later during normal loading
			*buf = b
		}
		return archiver.ErrStopWalk
	}
}

func filePath(f archiver.File) string {
	switch fh := f.Header.(type) {
	case zip.FileHeader:
		return filepath.Clean(fh.Name)
	case rardecode.FileHeader:
		return filepath.Clean(fh.Name)
	default:
		return filepath.Clean(f.Name())
	}
}
