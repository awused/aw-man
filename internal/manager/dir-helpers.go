package manager

import (
	"io/ioutil"
	"math"
	"os"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"

	"github.com/awused/aw-man/internal/natsort"
	log "github.com/sirupsen/logrus"
)

func findImagesInDir(dir string, paths *[]string) {
	// Use Readdirnames instead of ioutil.ReadDir for speed, especially for large remote directories.
	// We don't need to stat the files, which can cost over a second on a few thousand files.
	// If you name a directory "a.jpg" you deserve to see an error.
	d, err := os.Open(dir)
	if err != nil {
		log.Errorln("Error listing files in directory", dir, err)
	}
	defer d.Close()

	files, err := d.Readdirnames(-1)
	if err != nil {
		log.Errorln("Error listing files in directory", dir, err)
		return
	}

	for _, f := range files {
		if !isNativelySupportedImage(f) {
			continue
		}

		*paths = append(*paths, f)
	}
}

var mangaSyncerFileRegex = regexp.MustCompile(
	`^(Vol\. [^ ]+ )?Ch\. ([^ ]+) .* - [a-zA-Z0-9_-]+\.zip`)

func lessThan(ns natsort.NaturalSorter, a, b string) bool {
	if a == b {
		return false
	}

	ma := mangaSyncerFileRegex.FindStringSubmatch(a)
	if ma != nil {
		mb := mangaSyncerFileRegex.FindStringSubmatch(b)
		if mb != nil {
			ca, ea := strconv.ParseFloat(ma[2], 32)
			cb, eb := strconv.ParseFloat(mb[2], 32)
			if ea == nil && eb == nil && ca != math.NaN() && cb != math.NaN() {
				if ca < cb {
					return true
				} else if ca > cb {
					return false
				}
			}
		}
	}

	return ns.Compare(a, b)
}

// findBeforeAndAfterInDir finds the previous and next archives inside the directory.
func findBeforeAndAfterInDir(file string, dir string) (string, string) {
	before := ""
	after := ""
	files, err := ioutil.ReadDir(dir)
	if err != nil {
		log.Errorln("Error listing files in directory", dir, err)
		return "", ""
	}

	ns := natsort.NewNaturalSorter()
FileLoop:
	for _, fi := range files {
		if fi.IsDir() || file == fi.Name() {
			continue
		}

		switch strings.ToLower(filepath.Ext(fi.Name())) {
		case ".zip":
		case ".rar":
		case ".cbz":
		case ".cbr":
		case ".7z":
		default:
			continue FileLoop
		}

		if lessThan(ns, fi.Name(), file) {
			if before == "" || lessThan(ns, before, fi.Name()) {
				before = fi.Name()
			}
		} else {
			if after == "" || lessThan(ns, fi.Name(), after) {
				after = fi.Name()
			}
		}
	}

	log.Debugln(before, "<", file, "<", after)
	return before, after
}
