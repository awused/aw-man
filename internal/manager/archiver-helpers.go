package manager

import (
	"io"
	"io/ioutil"
	"os"
	"path/filepath"
	"strconv"

	"github.com/awused/aw-manga/internal/closing"
	"github.com/mholt/archiver/v3"
	log "github.com/sirupsen/logrus"
)

func archiverDiscovery(
	pages *[]*page,
	extractionMap map[string]*page) archiver.WalkFunc {

	return func(f archiver.File) error {
		select {
		case <-closing.Ch:
			return archiver.ErrStopWalk
		default:
		}

		if f.IsDir() || !isImage(f.Name()) {
			return nil
		}

		p := newPage(filePath(f))
		if _, ok := extractionMap[p.archivePath]; ok {
			log.Errorln("Duplicate path in archive", p.archivePath)
			return nil
		}

		*pages = append(*pages, p)
		extractionMap[p.archivePath] = p

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

		path := filepath.Clean(filePath(f))

		p, ok := extractionMap[path]
		if !ok {
			return nil
		}
		defer close(p.extractCh)
		delete(extractionMap, path)

		outPath := strconv.Itoa(p.number) + filepath.Ext(path)
		outPath = filepath.Join(a.tmpDir, outPath)

		outF, err := os.Create(outPath)
		if err != nil {
			log.Errorln("Error creating output file", a, path, outPath, err)
			return nil
		}
		defer outF.Close()

		_, err = io.Copy(outF, f.ReadCloser)
		if err != nil {
			log.Errorln("Error extracting file", a, path, outPath, err)
			return nil
		}

		p.extractCh <- outPath

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
		if path != page.archivePath {
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
