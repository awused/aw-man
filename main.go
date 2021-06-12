package main

import (
	"flag"
	"image"
	"io/ioutil"
	"os"
	"os/signal"
	"path/filepath"
	"sync"
	"syscall"

	"net/http"
	_ "net/http/pprof"

	log "github.com/sirupsen/logrus"

	"gioui.org/app"
	"gioui.org/unit"
	"github.com/awused/aw-manga/internal/closing"
	"github.com/awused/aw-manga/internal/config"
	"github.com/awused/aw-manga/internal/manager"
	"github.com/awused/awconf"
)

func main() {
	closing.Ch = make(chan struct{})

	flag.Parse()

	if *config.DebugFlag {
		log.SetLevel(log.DebugLevel)
		go func() {
			log.Errorln(http.ListenAndServe("localhost:6060", nil))
		}()
	}

	if flag.NArg() != 1 {
		log.Fatalln("Provide exactly one archive to load")
	}
	firstArchive, err := filepath.Abs(flag.Arg(0))
	if err != nil {
		log.Fatalln(firstArchive, "is not a valid path", err)
	}
	fi, err := os.Stat(firstArchive)
	if err != nil || fi.IsDir() {
		log.Fatalln(firstArchive, "is not a valid file", err)
	}

	err = awconf.LoadConfig("aw-manga", &config.Conf)
	if err != nil {
		log.Fatalln(err)
	}
	config.Sanity()

	sigs := make(chan os.Signal, 1)
	signal.Notify(sigs, syscall.SIGINT, syscall.SIGTERM)

	tmpDir, err := ioutil.TempDir(config.Conf.TempDirectory, "aw-manga*")
	if err != nil {
		log.Fatalln(err)
	}
	defer os.RemoveAll(tmpDir)

	wg := &sync.WaitGroup{}
	commandChan := make(chan manager.Command)
	sizeChan := make(chan image.Point)
	stateChan := make(chan manager.State)

	wg.Add(3)

	go (&gui{
		commandChan: commandChan,
		sizeChan:    sizeChan,
		stateChan:   stateChan,
		window: app.NewWindow(
			app.Title("aw-manga"),
			app.MinSize(unit.Dp(100), unit.Dp(100))),
	}).run(wg)
	go manager.NewManager(
		commandChan, sizeChan, stateChan, tmpDir).Run(wg, flag.Arg(0))

	go func() {
		defer wg.Done()

		select {
		case <-sigs:
			closing.Once()
		case <-closing.Ch:
		}
	}()
	go func() {
		wg.Wait()
		os.RemoveAll(tmpDir)
		os.Exit(0)
	}()

	app.Main()
}
