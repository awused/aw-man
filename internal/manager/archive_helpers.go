package manager

import (
	"archive/zip"
	"io"
	"os"
	"path/filepath"
	"sync"

	"github.com/awused/aw-man/internal/closing"
	"github.com/awused/aw-man/internal/sevenzip"
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
	files, err := sevenzip.ListFiles(path)
	if err != nil {
		log.Errorln("Error opening 7z archive", err)
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
	files, err := sevenzip.ListFiles(a.path)
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
	// There are diminishing returns and increasing memory usage, so stick to 4 threads.
	sem := make(chan struct{}, 4)

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
		case sem <- struct{}{}:
		}

		buf := make([]byte, file.Size)
		_, err := io.ReadFull(readCloser, buf)
		if err != nil {
			log.Errorln("Error extracting from 7z archive", err)
			return
		}
		delete(extractionMap, file.Path)

		wg.Add(1)
		go func(file sevenzip.SevenZipFile) {
			defer func() { <-sem }()
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
