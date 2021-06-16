package manager

import (
	"io/ioutil"

	log "github.com/sirupsen/logrus"
)

func walkDir(dir string, paths *[]string) {
	files, err := ioutil.ReadDir(dir)
	if err != nil {
		log.Errorln("Error reading file in directory", dir, err)
	}

	for _, fi := range files {
		if fi.IsDir() || !isNativelySupportedImage(fi.Name()) {
			continue
		}

		*paths = append(*paths, fi.Name())
	}
}
