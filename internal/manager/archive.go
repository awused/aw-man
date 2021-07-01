package manager

import (
	"fmt"
	"io/ioutil"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"sync"
	"time"

	"github.com/awused/aw-man/internal/natsort"
	"github.com/awused/aw-man/internal/pixbuf"
	"github.com/awused/aw-man/internal/vips"
	"github.com/mholt/archiver/v3"
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

type openType int8

const (
	preloading openType = iota
	// Used to prioritize extracting the first page out of an archive.
	waitingOnFirst
	// Used to prioritize extracting the last page out of an archive.
	waitingOnLast
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
	// Name is base name of the archive or directory.
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
		log.Infoln("Finished closing", a)
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

// Synchronously opens an archive, lists all of its files, filters them,
// and then begins asynchronously extracting them.
// Could be made entirely asynchronous but it's likely not worth it.
// Returns the index of the first page to display.
func openArchive(
	file string,
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

	paths := []string{}
	extractionMap := make(map[string]*page)

	ext := strings.ToLower(filepath.Ext(file))
	if ext == ".zip" || ext == ".cbz" {
		err = archiver.DefaultZip.Walk(file, archiverDiscovery(&paths))
		if err != nil {
			log.Errorln(err)
		} else {
			a.kind = zipArchive
		}
	} else if ext == ".rar" || ext == ".cbr" {
		err = archiver.DefaultRar.Walk(file, archiverDiscovery(&paths))
		if err != nil {
			log.Errorln(err)
		} else {
			a.kind = rarArchive
		}
	} else if isSupportedImage(file) {
		a.kind = directory

		a.path = filepath.Dir(a.path)
		a.name = filepath.Base(a.path)
	} else if d, err := os.Stat(file); err == nil && d.IsDir() {
		a.kind = directory
	}

	if a.kind == directory {
		paths = findImagesInDir(a.path)
	}

	if a.kind == unknown && (ext == ".cbz" || ext == ".7z" || ext == ".cb7") {
		paths, err = sevenZipDiscovery(a.path)
		if err == nil {
			a.kind = sevenZipArchive
		}
	}
	if len(paths) == 0 {
		log.Errorln("Could not find any images in archive", a)
	}

	ns := natsort.NewNaturalSorter()
	sort.Slice(paths, func(i, j int) bool {
		return ns.Compare(paths[i], paths[j])
	})

	for i, path := range paths {
		if a.kind == directory && filepath.Join(a.path, path) == file {
			initialPage = i
		}

		var p *page
		if a.kind != directory {
			p = newArchivePage(path, i, a.tmpDir)
			extractionMap[p.inArchivePath] = p
		} else {
			p = newDirectoryPage(path, a.path, i, a.tmpDir)
		}
		a.pages = append(a.pages, p)
	}

	// Trim common prefixes from the name displayed to the user.
	trimCommonNamePrefix(a.pages)

	log.Infoln("Scanned", a)

	var fastPage *page

	// Extract the desired page first synchronously. If we're not upscaling, load it.
	if len(a.pages) > 0 && trigger != preloading {
		if trigger == waitingOnLast {
			initialPage = len(a.pages) - 1
		}
		fastPage = a.pages[initialPage]
	}

	go func() {
		defer close(extracting)
		defer func() {
			if a.kind != directory {
				log.Infoln("Finished extracting", a)
			}
			// Finalize any pending extractions on early close or if the files were
			// somehow missing.
			for _, p := range extractionMap {
				close(p.extractCh)
			}
		}()

		if fastPage != nil {
			switch a.kind {
			case zipArchive:
				archiver.DefaultZip.Walk(a.path, archiverExtractor(a, extractionMap, fastPage))
			case rarArchive:
				archiver.DefaultRar.Walk(a.path, archiverExtractor(a, extractionMap, fastPage))
			case sevenZipArchive:
				sevenZipExtractTargetPage(a, extractionMap, fastPage)
			case directory:
				// Nothing needs to be done here
			}
		}

		switch a.kind {
		case zipArchive:
			archiver.DefaultZip.Walk(a.path, archiverExtractor(a, extractionMap, nil))
		case rarArchive:
			archiver.DefaultRar.Walk(a.path, archiverExtractor(a, extractionMap, nil))
		case sevenZipArchive:
			sevenZipExtract(a, extractionMap)
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

func isSupportedImage(f string) bool {
	return isNativelySupportedImage(f) || vips.IsSupportedImage(f) || pixbuf.IsSupportedImage(f)
}

func isNativelySupportedImage(f string) bool {
	e := strings.ToLower(filepath.Ext(f))
	return e == ".png" || e == ".jpg" || e == ".jpeg" || e == ".webp" || e == ".tiff" || e == ".tif"
}
