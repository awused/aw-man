package config

import (
	"flag"
	"os"

	log "github.com/sirupsen/logrus"
)

type config struct {
	TempDirectory           string
	Preload                 int
	Retain                  int
	LoadThreads             int
	Waifu2xNCNNVulkan       string
	Waifu2xNCNNVulkanModels string
}

var Conf config

var MangaMode = flag.Bool(
	"manga",
	false,
	"Treat the directory containing the archive as if it contains an entire "+
		"series of manga chapters or volumes. Figuring out the order of the "+
		"archives is best-effort.")
var DebugFlag = flag.Bool(
	"debug",
	false,
	"Serve debugging information at http://localhost:6060/debug/pprof")

func Sanity() {
	rootTDir := Conf.TempDirectory
	if rootTDir == "" {
		rootTDir = os.TempDir()
		if rootTDir == "" {
			log.Fatalln("No temp directory configured and no default temp directory.")
		}
		Conf.TempDirectory = rootTDir
	}

	if Conf.Preload < 0 || Conf.Retain < 0 || Conf.LoadThreads < 1 {
		log.Fatalln(
			"Must have at least one thread and non-negative preloading/retaining")
	}
}
