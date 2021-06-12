package manager

import (
	"archive/zip"
	"bytes"
	"fmt"
	"image"
	"io"
	"io/ioutil"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	_ "image/jpeg"
	_ "image/png"

	_ "golang.org/x/image/webp"

	"github.com/facette/natsort"
	"github.com/mholt/archiver/v3"
	"github.com/nwaples/rardecode"
	log "github.com/sirupsen/logrus"
)

var startTime time.Time = time.Now()

type archive struct {
	name       string
	file       string
	tmpDir     string
	closed     chan struct{}
	extracting chan struct{}
	pages      []*page
}

func (a *archive) String() string {
	extracted := false
	select {
	case <-a.extracting:
		extracted = true
	default:
	}
	return fmt.Sprintf(
		"[a:%s %d %t]",
		a.file,
		len(a.pages),
		extracted)
}

func (a *archive) close(wg *sync.WaitGroup) {
	close(a.closed)
	wg.Add(1)
	go func() {
		defer wg.Done()
		<-a.extracting

		for _, p := range a.pages {
			p.cleanup()
		}
		os.RemoveAll(a.tmpDir)
		log.Debugln("Finished closing", a)
	}()
}

type openType int8

const (
	preloading openType = iota
	// Used to sycnhronously extract the first or last image if the UI is blocked
	// on them.
	waitingOnFirst
	waitingOnLast
)

type archiveKind int8

const (
	zipArchive archiveKind = iota
	rarArchive
	sevenZipArchive
)

// Synchronously opens an archive, lists all of its files, filters them,
// and then begins asynchronously extracting them.
// Could be made entirely asynchronous but it's likely not worth it.
func openArchive(
	file string,
	targetSize image.Point,
	tdir string,
	trigger openType) *archive {
	tmpDir, err := ioutil.TempDir(tdir, "chapter*")
	if err != nil {
		log.Errorln(err)
		return nil
	}
	extracting := make(chan struct{})
	a := &archive{
		file:       file,
		name:       filepath.Base(file),
		closed:     make(chan struct{}),
		extracting: extracting,
		tmpDir:     tmpDir,
	}

	pages := []*page{}
	fileMap := make(map[string]chan<- string)

	kind := zipArchive

	ext := strings.ToLower(filepath.Ext(file))
	if ext == ".zip" || ext == ".cbz" {
		err = archiver.DefaultZip.Walk(file, archiverDiscovery(&pages, fileMap))
		if err != nil {
			log.Errorln(err)
		}
	} else if ext == ".rar" || ext == ".cbr" {
		kind = rarArchive
		err = archiver.DefaultRar.Walk(file, archiverDiscovery(&pages, fileMap))
		if err != nil {
			log.Errorln(err)
		}
	}
	// TODO -- 7z, cbz but 7zip

	if len(pages) == 0 {
		log.Errorln("Could not find any images in archive", a)
	}

	// Remove longest common directory prefix from extractableImage names
	trimCommonNamePrefix(pages)
	// TODO -- Sort
	sort.Slice(pages, func(i, j int) bool {
		return natsort.Compare(pages[i].name, pages[j].name)
	})
	a.pages = pages
	log.Debugln("Found images in archive", a, pages)

	if len(pages) > 0 && trigger != preloading {
		fastPage := pages[0]
		if trigger == waitingOnLast {
			fastPage = pages[len(pages)-1]
		}

		syncLoad(a, targetSize, kind, fastPage, fileMap)
	}

	go func() {
		defer close(extracting)
		defer func() {
			// Finalize any extractions on early close or that were somehow missing.
			for _, c := range fileMap {
				close(c)
			}
		}()

		if kind == zipArchive {
			archiver.DefaultZip.Walk(file, archiverExtractor(a, fileMap))
		} else if kind == rarArchive {
			archiver.DefaultRar.Walk(file, archiverExtractor(a, fileMap))
		}
	}()

	return a
}

// Trim out any directories common to all files
// a/b/c.png becomes b/c.png if all files are in a/
func trimCommonNamePrefix(pages []*page) {
	if len(pages) == 0 {
		return
	}

	prefix := filepath.Dir(pages[0].name)
	for _, p := range pages[1:] {
		d := filepath.Dir(p.name)
		if d == "." {
			return
		}

		for prefix != d {
			if d == "." || prefix == "." || len(prefix) == 1 && os.IsPathSeparator(prefix[0]) {
				prefix = ""
				break
			}
			if len(prefix) > len(d) {
				prefix = filepath.Dir(prefix)
			} else if len(prefix) < len(d) {
				d = filepath.Dir(d)
			} else {
				prefix = filepath.Dir(prefix)
				d = filepath.Dir(d)
			}
		}
	}

	for _, p := range pages {
		p.name = strings.TrimPrefix(p.name, prefix)
		if len(p.name) > 0 && os.IsPathSeparator(p.name[0]) {
			p.name = p.name[1:]
		}
	}
}

func syncLoad(
	a *archive,
	targetSize image.Point,
	kind archiveKind,
	page *page,
	fileMap map[string]chan<- string) {
	log.Debugln("Extracting page early", page, time.Now().Sub(startTime))

	ch, ok := fileMap[page.archivePath]
	if !ok {
		// This should never happen, just die
		log.Panicln(
			"Tried to syncLoad page not present in fileMap", page.archivePath)
	}

	buf := []byte{}
	//var err error

	if kind == zipArchive {
		archiver.DefaultZip.Walk(a.file, archiverByteFetcher(page, &buf))
	} else if kind == rarArchive {
		archiver.DefaultRar.Walk(a.file, archiverByteFetcher(page, &buf))
	}

	if len(buf) == 0 {
		return
	}

	fastFile := filepath.Join(
		a.tmpDir, "0"+filepath.Ext(page.archivePath))
	li := loadableFromBytes(fastFile, targetSize, buf)
	if li == nil {
		return
	}

	// We need to write the file synchronously too, otherwise we break the
	// contract. It is possible to write the file asynchronously (at least
	// allowing for updating the UI before writing) by returning
	// another channel, but it's more complicated.
	// If writing the file fails or takes too long, we have a "loadableImage"
	// that is not loadable.
	f, err := os.Create(fastFile)
	if err != nil {
		// 	// Ignore the error and report it normally later
		log.Debugln("Early extraction failed", page, err)
		return
	}
	defer f.Close()

	_, err = io.Copy(f, bytes.NewReader(buf))
	if err != nil {
		// Ignore the error and report it normally later
		log.Debugln("Early extraction failed", page, err)
		return
	}

	// Everything has succeeded, we are now safe to mark it as extracted
	close(ch)
	delete(fileMap, page.archivePath)
	page.state = extracted
	page.normal = li
	log.Debugln("Extracted page early", page, time.Now().Sub(startTime))
}

func filePath(f archiver.File) string {
	switch fh := f.Header.(type) {
	case zip.FileHeader:
		return fh.Name
	case rardecode.FileHeader:
		return fh.Name
	default:
		return f.Name()
	}
}

func isImage(f string) bool {
	e := strings.ToLower(filepath.Ext(f))
	return e == ".png" || e == ".jpg" || e == ".jpeg" || e == ".webp"
}
