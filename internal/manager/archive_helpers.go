package manager

import (
	"archive/zip"
	"errors"
	"io"
	"os"
	"path/filepath"
	"strings"
	"sync"

	"github.com/awused/aw-man/internal/closing"
	"github.com/awused/aw-man/internal/sevenzip"
	"github.com/awused/aw-man/internal/unrar"
	"github.com/mholt/archiver/v3"
	"github.com/nwaples/rardecode"
	log "github.com/sirupsen/logrus"
)

var extractionSem chan struct{}

func archiverDiscovery(paths *[]string) archiver.WalkFunc {
	return func(f archiver.File) error {
		select {
		case <-closing.Ch:
			return archiver.ErrStopWalk
		default:
		}

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

func sevenZipDiscovery(path string) ([]string, archiveKind, error) {
	ak := unknown
	files, kind, err := sevenzip.GetMetadata(path)
	if err != nil {
		log.Errorln("Error opening archive with 7z", err)
		return nil, unknown, err
	}

	out := []string{}
	for _, file := range files {
		if isSupportedImage(file.Path) {
			out = append(out, file.Path)
		}
	}

	if kind == "zip" {
		ak = zipArchive
	} else if kind == "7z" {
		ak = sevenZipArchive
	} else if strings.HasPrefix(kind, "rar") {
		ak = rarArchive
	} else {
		err = errors.New("Unexpected archive format: " + kind)
		log.Errorln("Error opening archive with 7z", err)
		return nil, unknown, err
	}

	return out, ak, nil
}

func sevenZipExtractTargetPage(
	a *archive,
	extractionMap map[string]*page,
	targetPage *page) {
	if targetPage == nil {
		return
	}

	path := targetPage.inArchivePath
	p, ok := extractionMap[path]
	if !ok {
		return
	}
	delete(extractionMap, path)
	defer close(p.extractCh)

	err := sevenzip.ExtractFile(a.path, path, targetPage.file)
	if err != nil {
		log.Errorln("Error extracting file.", a, path, p.file, err)
		p.extractCh <- false
		return
	}
	p.extractCh <- true
}

func sevenZipExtract(
	a *archive,
	extractionMap map[string]*page) {
	// Somewhat wasteful to read the list of files again, but not worth eliminating.
	files, _, err := sevenzip.GetMetadata(a.path)
	if err != nil {
		log.Errorln("Error opening 7z archive", err)
		return
	}

	readCloser, err := sevenzip.GetReader(a.path)
	if err != nil {
		log.Errorln("Error opening 7z archive", err)
		return
	}
	defer readCloser.Close()

	wg := sync.WaitGroup{}
	defer wg.Wait()

	for _, file := range files {
		select {
		case <-closing.Ch:
			return
		case <-a.closed:
			return
		default:
		}

		p, ok := extractionMap[file.Path]
		if !ok {
			_, err = io.CopyN(io.Discard, readCloser, file.Size)
			if err != nil {
				log.Errorln("Error extracting from 7z archive", err)
				return
			}
			continue
		}

		select {
		case <-closing.Ch:
			return
		case <-a.closed:
			return
		case extractionSem <- struct{}{}:
		}

		buf := make([]byte, file.Size)
		_, err := io.ReadFull(readCloser, buf)
		if err != nil {
			log.Errorln("Error extracting from 7z archive", err)
			<-extractionSem
			return
		}
		delete(extractionMap, file.Path)

		wg.Add(1)
		go func(file sevenzip.File) {
			defer func() { <-extractionSem }()
			defer wg.Done()
			success := false

			defer func() {
				// We must send to the channel after the file has closed
				p.extractCh <- success
				close(p.extractCh)
			}()

			err = os.WriteFile(p.file, buf, 0666)
			if err != nil {
				log.Errorln("Error extracting file", a, file.Path, p.file, err)
				return
			}

			success = true
		}(file)
	}
}

func unrarDiscovery(path string) ([]string, error) {
	files, err := unrar.GetMetadata(path)
	if err != nil {
		log.Errorln("Error opening archive with unrar", err)
		return nil, err
	}

	out := []string{}
	for _, file := range files {
		if isSupportedImage(file.Path) {
			out = append(out, file.Path)
		}
	}

	return out, nil
}

func unrarExtractTargetPage(
	a *archive,
	extractionMap map[string]*page,
	targetPage *page) {
	if targetPage == nil {
		return
	}

	path := targetPage.inArchivePath
	p, ok := extractionMap[path]
	if !ok {
		return
	}
	delete(extractionMap, path)
	defer close(p.extractCh)

	err := unrar.ExtractFile(a.path, path, targetPage.file)
	if err != nil {
		log.Errorln("Error extracting file.", a, path, p.file, err)
		p.extractCh <- false
		return
	}
	p.extractCh <- true
}

func unrarExtract(
	a *archive,
	extractionMap map[string]*page) {
	// Somewhat wasteful to read the list of files again, but not worth eliminating.
	files, err := unrar.GetMetadata(a.path)
	if err != nil {
		log.Errorln("Error opening rar archive", err)
		return
	}

	readCloser, err := unrar.GetReader(a.path)
	if err != nil {
		log.Errorln("Error opening rar archive", err)
		return
	}
	defer readCloser.Close()

	wg := sync.WaitGroup{}
	defer wg.Wait()

	for _, file := range files {
		select {
		case <-closing.Ch:
			return
		case <-a.closed:
			return
		default:
		}

		p, ok := extractionMap[file.Path]
		if !ok {
			_, err = io.CopyN(io.Discard, readCloser, file.Size)
			if err != nil {
				log.Errorln("Error extracting from rar archive", err)
				return
			}
			continue
		}

		select {
		case <-closing.Ch:
			return
		case <-a.closed:
			return
		case extractionSem <- struct{}{}:
		}

		buf := make([]byte, file.Size)
		_, err := io.ReadFull(readCloser, buf)
		if err != nil {
			log.Errorln("Error extracting from rar archive", err)
			<-extractionSem
			return
		}
		delete(extractionMap, file.Path)

		wg.Add(1)
		go func(file unrar.File) {
			defer func() { <-extractionSem }()
			defer wg.Done()
			success := false

			defer func() {
				// We must send to the channel after the file has closed
				p.extractCh <- success
				close(p.extractCh)
			}()

			err = os.WriteFile(p.file, buf, 0666)
			if err != nil {
				log.Errorln("Error extracting file", a, file.Path, p.file, err)
				return
			}

			success = true
		}(file)
	}
	wg.Wait()
}
