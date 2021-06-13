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
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/facette/natsort"
	"github.com/mholt/archiver/v3"
	"github.com/nwaples/rardecode"
	log "github.com/sirupsen/logrus"
)

var startTime time.Time = time.Now()

type archiveKind int8

const (
	zipArchive archiveKind = iota
	rarArchive
	sevenZipArchive
	directory
	unknown
)

var kindNames = map[archiveKind]string{
	zipArchive:      "zip",
	rarArchive:      "rar",
	sevenZipArchive: "7z",
	directory:       "dir",
	unknown:         "unknown",
}

func (ak archiveKind) String() string {
	return kindNames[ak]
}

type archive struct {
	name       string
	kind       archiveKind
	path       string
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
		a.path,
		len(a.pages),
		extracted)
}

func (a *archive) Close(wg *sync.WaitGroup) {
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

// Get returns the page and the relevant loadableImage.
// Page will be nil iff this is an invalid archive or it contains no images.
func (a *archive) Get(i int, upscaling bool) (*page, *loadableImage) {
	if a.PageCount() <= i || i < 0 {
		if i != 0 {
			log.Panicf(
				"Tried to get %d but archive %s does not have that image\n", i, a)
		}
		return nil, nil
	}

	p := a.pages[i]
	return p, p.Get(upscaling)
}

// PageCount counts the pages in the archive
func (a *archive) PageCount() int {
	return len(a.pages)
}

type openType int8

const (
	preloading openType = iota
	// Used to sycnhronously extract the first or last image if the UI is blocked
	// on them.
	waitingOnFirst
	waitingOnLast
)

// Synchronously opens an archive, lists all of its files, filters them,
// and then begins asynchronously extracting them.
// Could be made entirely asynchronous but it's likely not worth it.
// Returns the index of the first page to display.
func openArchive(
	file string,
	bounds image.Point,
	tdir string,
	trigger openType,
	upscaling bool) (*archive, int) {
	tmpDir, err := ioutil.TempDir(tdir, "chapter*")
	if err != nil {
		log.Panicln("Error creating temp directory for", file, err)
	}
	initialPage := 0

	extracting := make(chan struct{})
	a := &archive{
		path:       file,
		kind:       unknown,
		name:       filepath.Base(file),
		closed:     make(chan struct{}),
		extracting: extracting,
		tmpDir:     tmpDir,
	}

	extractionMap := make(map[string]*page)

	ext := strings.ToLower(filepath.Ext(file))
	if ext == ".zip" || ext == ".cbz" {
		err = archiver.DefaultZip.Walk(file, archiverDiscovery(&a.pages, extractionMap))
		if err != nil {
			log.Errorln(err)
		} else {
			a.kind = zipArchive
		}
	} else if ext == ".rar" || ext == ".cbr" {
		err = archiver.DefaultRar.Walk(file, archiverDiscovery(&a.pages, extractionMap))
		if err != nil {
			log.Errorln(err)
		} else {
			a.kind = rarArchive
		}
	} else if isImage(file) {
		a.kind = directory

		a.path = filepath.Dir(a.path)
		a.name = filepath.Base(a.path)
	} else if d, err := os.Stat(file); err == nil && d.IsDir() {
		a.kind = directory
	}

	if a.kind == directory {
		walkDir(&a.pages)
	}

	if a.kind == unknown && (ext == ".cbz" || ext == ".7z") {
	}
	// TODO -- 7z, cbz but 7zip

	if len(a.pages) == 0 {
		log.Errorln("Could not find any images in archive", a)
	}

	// Remove longest common directory prefix from extractableImage names
	trimCommonNamePrefix(a.pages)
	sort.Slice(a.pages, func(i, j int) bool {
		return natsort.Compare(a.pages[i].name, a.pages[j].name)
	})
	for i, p := range a.pages {
		p.number = i
		if a.kind == directory && filepath.Join(a.path, p.archivePath) == file {
			initialPage = i
		}
	}

	log.Debugln("Scanned archive", a)

	// Extract the desired page synchronously. If we're not upscaling, load it.
	if len(a.pages) > 0 && trigger != preloading {
		if trigger == waitingOnLast {
			initialPage = len(a.pages) - 1
		}
		fastPage := a.pages[initialPage]

		syncExtractMaybeLoad(a, bounds, fastPage, extractionMap, upscaling)
	}

	go func() {
		defer close(extracting)
		defer func() {
			// Finalize any pending extractions on early close or if the files were
			// somehow missing.
			for _, p := range extractionMap {
				close(p.extractCh)
			}
		}()

		switch a.kind {
		case zipArchive:
			archiver.DefaultZip.Walk(a.path, archiverExtractor(a, extractionMap))
		case rarArchive:
			archiver.DefaultRar.Walk(a.path, archiverExtractor(a, extractionMap))
		case sevenZipArchive:
		case directory:
			// Nothing needs to be done here
		}
	}()

	return a, initialPage
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

func syncExtractMaybeLoad(
	a *archive,
	bounds image.Point,
	p *page,
	extractionMap map[string]*page,
	upscaling bool) {
	log.Debugln("Extracting page early", p, time.Now().Sub(startTime))

	_, ok := extractionMap[p.archivePath]
	if !ok {
		// This should never happen, just die
		log.Panicln(
			"Tried to syncLoad page not present in extractionMap", p.archivePath)
	}

	buf := []byte{}

	switch a.kind {
	case zipArchive:
		archiver.DefaultZip.Walk(a.path, archiverByteFetcher(p, &buf))
	case rarArchive:
		archiver.DefaultRar.Walk(a.path, archiverByteFetcher(p, &buf))
	case sevenZipArchive:
	case directory:
		// It's not necessary to do anything for directories here.
		if p.normal == nil {
			// This should never happen
			log.Panicln("Tried to load nil image from directory", p)
		}
		p.normal.loadSync(bounds, false)
		return
	}

	if len(buf) == 0 {
		return
	}

	fastFile := filepath.Join(
		a.tmpDir, strconv.Itoa(p.number)+filepath.Ext(p.archivePath))
	li := loadableFromBytes(fastFile, bounds, buf)
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
		log.Debugln("Early extraction failed", p, err)
		return
	}
	defer f.Close()

	_, err = io.Copy(f, bytes.NewReader(buf))
	if err != nil {
		// Ignore the error and report it normally later
		log.Debugln("Early extraction failed", p, err)
		return
	}

	// Everything has succeeded, we are now safe to mark it as extracted
	close(p.extractCh)
	delete(extractionMap, p.archivePath)
	p.state = extracted
	p.normal = li
	log.Debugln("Extracted page early", p, time.Now().Sub(startTime))
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
