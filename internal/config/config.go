package config

import (
	"flag"
	"os"
	"path/filepath"

	"github.com/awused/awconf"
	log "github.com/sirupsen/logrus"
)

type config struct {
	TempDirectory     string
	Preload           int
	Retain            int
	LoadThreads       int
	AlternateUpscaler string
}

// Conf is the single global config state
var Conf config

// MangaMode controls if the application should start in manga mode.
var MangaMode bool

// UpscaleMode controls if the application should start with upscaling enabled.
var UpscaleMode bool

const mangaUsage = "Start the program in manga mode, enabling continuous " +
	"scrolling through the current directory."
const upscaleUsage = "Start the program with upscaling enabled."

// DebugFlag tracks if the debugging interface is active.
var DebugFlag = flag.Bool(
	"debug",
	false,
	"Serve debugging information at http://localhost:6060/debug/pprof")

func init() {
	flag.BoolVar(&MangaMode, "m", false, mangaUsage)
	flag.BoolVar(&MangaMode, "manga", false, mangaUsage)
	flag.BoolVar(&UpscaleMode, "u", false, upscaleUsage)
	flag.BoolVar(&UpscaleMode, "upscale", false, upscaleUsage)
}

// Load initializes the config and crashes the program if the config is
// obviously invalid.
func Load() {
	flag.Parse()

	err := awconf.LoadConfig("aw-man", &Conf)
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
