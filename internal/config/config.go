package config

import (
	"flag"
	"image"
	"os"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"

	"github.com/awused/awconf"
	log "github.com/sirupsen/logrus"
)

type config struct {
	TargetResolution string
	TempDirectory    string
	PreloadAhead     int
	PreloadBehind    int
	LoadThreads      int
	Prescale         int
	MaximumUpscaled  int

	AlternateUpscaler       string
	UpscalePreviousChapters bool
}

// UpscalingRest is the target resolution for upscaling that the user has configured.
// If this is (0, 0) then upscaling is entirely disabled.
var UpscalingRes = image.Point{}

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

	splitRes := strings.Split(Conf.TargetResolution, "x")
	if len(splitRes) != 2 {
		log.Fatalln("TargetResolution must be of the form WIDTHxHEIGHT. Example: 3840x2160.")
	}

	x, err := strconv.Atoi(splitRes[0])
	if err != nil {
		log.Fatalln("TargetResolution must be of the form WIDTHxHEIGHT. Example: 3840x2160.")
	}

	y, err := strconv.Atoi(splitRes[1])
	if err != nil {
		log.Fatalln("TargetResolution must be of the form WIDTHxHEIGHT. Example: 3840x2160.")
	}

	if x < 0 || y < 0 {
		log.Fatalln("Both dimensions of TargetResolution must be non-negative.")
	}
	if x != 0 && y != 0 {
		UpscalingRes = image.Point{X: x, Y: y}
	} else {
		log.Infoln("Upscaling disabled by TargetResolution setting")
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

	if Conf.PreloadAhead < 0 || Conf.PreloadBehind < 0 || Conf.LoadThreads < 0 ||
		Conf.Prescale < 0 || Conf.MaximumUpscaled < 0 {
		log.Fatalln(
			"Settings cannot be negative.")
	}

	if Conf.LoadThreads == 0 {
		Conf.LoadThreads = runtime.NumCPU() / 2
		if Conf.LoadThreads < 2 {
			Conf.LoadThreads = 2
		}
	}

}
