package manager

import (
	"archive/zip"
	"io"
	"os"
	"path/filepath"

	"github.com/awused/aw-man/internal/closing"
	"github.com/bodgit/sevenzip"
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

		// TODO -- magick supported images
		if f.IsDir() || !isSupportedImage(f.Name()) {
			return nil
		}

		p := filePath(f)
		*paths = append(*paths, p)

		return nil
	}
}

// If targetPage is not null, only extract that page.
func archiverExtractor(
	a *archive,
	extractionMap map[string]*page,
	targetPage *page) archiver.WalkFunc {

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
		if targetPage != nil && path != targetPage.inArchivePath {
			return nil
		}

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

		outF, err := os.Create(p.file)
		if err != nil {
			log.Errorln("Error creating output file", a, path, p.file, err)
			return nil
		}
		defer func() {
			if outF.Close() != nil {
				success = false
			}
		}()

		_, err = io.Copy(outF, f.ReadCloser)
		if err != nil {
			log.Errorln("Error extracting file", a, path, p.file, err)
			return nil
		}

		success = true
		//log.Debugln("Finished extracting", p)

		if targetPage != nil {
			return archiver.ErrStopWalk
		}
		return nil
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

func sevenZipDiscovery(path string) ([]string, error) {
	r, err := sevenzip.OpenReader(path)
	if err != nil {
		log.Errorln("Error opening 7z archive", err)
		return nil, err
	}
	defer r.Close()

	out := []string{}
	for _, file := range r.File {
		path := filepath.Clean(file.Name)
		if isSupportedImage(path) {
			out = append(out, path)
		}
	}
	return out, nil
}

func sevenZipExtract(
	a *archive,
	extractionMap map[string]*page,
	targetPage *page) {
	r, err := sevenzip.OpenReader(a.path)
	if err != nil {
		log.Errorln("Error opening 7z archive", err)
		return
	}
	defer r.Close()

	for _, file := range r.File {

		select {
		case <-closing.Ch:
			return
		case <-a.closed:
			return
		default:
		}
		success := false

		path := filepath.Clean(file.Name)
		if targetPage != nil && path != targetPage.inArchivePath {
			continue
		}

		p, ok := extractionMap[path]
		if !ok {
			continue
		}
		defer func() {
			// We must send to the channel after the file has closed
			p.extractCh <- success
			close(p.extractCh)
		}()
		delete(extractionMap, path)

		outF, err := os.Create(p.file)
		if err != nil {
			log.Errorln("Error creating output file", a, path, p.file, err)
			return
		}
		defer func() {
			if outF.Close() != nil {
				success = false
			}
		}()

		inF, err := file.Open()
		if err != nil {
			log.Errorln("Error extracting file", a, path, p.file, err)
			return
		}
		defer inF.Close()

		_, err = io.Copy(outF, inF)
		if err != nil {
			log.Errorln("Error extracting file", a, path, p.file, err)
			return
		}

		success = true
		//log.Debugln("Finished extracting", p)

		if targetPage != nil {
			return
		}
	}
}
