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
	fileMap map[string]chan<- string) archiver.WalkFunc {

	return func(f archiver.File) error {
		select {
		case <-closing.Ch:
			return archiver.ErrStopWalk
		default:
		}

		if f.IsDir() || !isImage(f.Name()) {
			return nil
		}

		path := filepath.Clean(filePath(f))
		exCh := make(chan string, 1)
		upCh := make(chan string, 1)

		if _, ok := fileMap[path]; ok {
			log.Errorln("Duplicate path %s in archive")
			return nil
		}

		*pages = append(*pages, &page{
			name:        path,
			archivePath: path,
			state:       extracting,
			extractCh:   exCh,
			upscaleCh:   upCh,
		})
		fileMap[path] = exCh

		return nil
	}
}

func archiverExtractor(
	a *archive,
	fileMap map[string]chan<- string) archiver.WalkFunc {
	i := 0

	return func(f archiver.File) error {
		select {
		case <-closing.Ch:
			return archiver.ErrStopWalk
		case <-a.closed:
			return archiver.ErrStopWalk
		default:
		}

		path := filepath.Clean(filePath(f))

		ch, ok := fileMap[path]
		if !ok {
			return nil
		}
		defer close(ch)
		delete(fileMap, path)
		i += 1

		outPath := strconv.Itoa(i) + filepath.Ext(path)
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

		//log.Debugln("Finished writing", path, outPath)
		ch <- outPath

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
