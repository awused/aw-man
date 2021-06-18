package manager

import (
	"fmt"
	"io/ioutil"
	"math"
	"path/filepath"
	"regexp"
	"strconv"
	"strings"

	"github.com/facette/natsort"
	log "github.com/sirupsen/logrus"
)

func findImagesInDir(dir string, paths *[]string) {
	files, err := ioutil.ReadDir(dir)
	if err != nil {
		log.Errorln("Error listing files in directory", dir, err)
		return
	}

	for _, fi := range files {
		if fi.IsDir() || !isNativelySupportedImage(fi.Name()) {
			continue
		}

		*paths = append(*paths, fi.Name())
	}
}

var mangaSyncerFileRegex = regexp.MustCompile(
	`$(Vol\. [^ ]+)?Ch\. ([^ ]+).* - [a-zA-Z_-]+\.zip`)

func lessThan(a, b string) bool {
	if a == b {
		return false
	}

	// This is going to re-run on the current archive N times.
	// Probably not worth memoizing.
	ma := mangaSyncerFileRegex.FindStringSubmatch(a)
	if ma != nil {
		mb := mangaSyncerFileRegex.FindStringSubmatch(b)
		if mb != nil {
			ca, ea := strconv.ParseFloat(ma[2], 32)
			cb, eb := strconv.ParseFloat(mb[2], 32)
			if ea != nil && eb != nil && ca != math.NaN() && cb != math.NaN() {
				if ca < cb {
					fmt.Println(a, b, ca, cb)
					return true
				} else if cb > ca {
					return false
				}
			}
		}
	}

	return natsort.Compare(a, b)
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

		if lessThan(fi.Name(), file) {
			if before == "" || lessThan(before, fi.Name()) {
				before = fi.Name()
			}
		} else {
			if after == "" || lessThan(fi.Name(), after) {
				after = fi.Name()
			}
		}
		log.Println(fi.Name(), before, "/", after)
	}

	log.Debugln(before, "<", file, "<", after)
	return before, after
}
