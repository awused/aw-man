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
	"time"

	"net/http"
	_ "net/http/pprof"

	log "github.com/sirupsen/logrus"

	"gioui.org/app"
	"github.com/awused/aw-manga/internal/closing"
	"github.com/awused/aw-manga/internal/config"
	"github.com/awused/aw-manga/internal/manager"
)

func main() {
	flag.Parse()

	config.Load()

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
	_, err = os.Stat(firstArchive)
	if err != nil {
		log.Fatalln(firstArchive, "is not a valid file or directory", err)
	}

	sigs := make(chan os.Signal, 1)
	signal.Notify(sigs, syscall.SIGINT, syscall.SIGTERM)

	tmpDir, err := ioutil.TempDir(config.Conf.TempDirectory, "aw-manga*")
	if err != nil {
		log.Fatalln(err)
	}
	defer os.RemoveAll(tmpDir)

	wg := &sync.WaitGroup{}
	commandChan := make(chan manager.Command, 1)
	sizeChan := make(chan image.Point)
	stateChan := make(chan manager.State)

	wg.Add(3)

	go (&gui{
		commandChan: commandChan,
		sizeChan:    sizeChan,
		stateChan:   stateChan,
		window: app.NewWindow(
			app.Title("aw-manga"),
		),
	}).run(wg)
	go manager.RunManager(
		commandChan, sizeChan, stateChan, tmpDir, wg, firstArchive)

	go func() {

		select {
		case <-sigs:
			closing.Once()
		case <-closing.Ch:
		}
		wg.Done()

		<-time.After(20 * time.Second)
		os.RemoveAll(tmpDir)
		if *config.DebugFlag {
			log.Errorln("Failed to exit in a timely manner:",
				"http://localhost:6060/debug/pprof/goroutine?debug=1")
		} else {
			log.Fatalln("Failed to exit in a timely manner")
		}
	}()
	go func() {
		wg.Wait()
		os.RemoveAll(tmpDir)
		os.Exit(0)
	}()

	app.Main()
}
