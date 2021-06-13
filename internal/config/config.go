package config

import (
	"flag"
	"os"
	"path/filepath"

	"github.com/awused/awconf"
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

// Conf is the single global config state
var Conf config

// MangaMode tracks if the current directory should be treated as a series of
// manga chapters.
var MangaMode = flag.Bool(
	"manga",
	false,
	"Treat the directory containing the archive as if it contains an entire "+
		"series of manga chapters or volumes. Figuring out the order of the "+
		"archives is best-effort.")

// DebugFlag tracks if the debugging interface is active.
var DebugFlag = flag.Bool(
	"debug",
	false,
	"Serve debugging information at http://localhost:6060/debug/pprof")

// Load initializes the config and crashes the program if the config is
// obviously invalid.
func Load() {
	err := awconf.LoadConfig("aw-manga", &Conf)
	if err != nil {
		log.Fatalln(err)
	}

	rootTDir := Conf.TempDirectory
	if rootTDir == "" {
		rootTDir = os.TempDir()
		if rootTDir == "" {
			log.Fatalln("No temp directory configured and no default temp directory.")
		}
	}
	Conf.TempDirectory, err = filepath.Abs(rootTDir)
	if err != nil {
		log.Fatalln("Error absolute path for temp directory", err)
	}

	if Conf.Preload < 0 || Conf.Retain < 0 || Conf.LoadThreads < 1 {
		log.Fatalln(
			"Must have at least one thread and non-negative preloading/retaining")
	}
}
