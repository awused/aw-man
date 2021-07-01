package main

import (
	"flag"
	"image"
	"io/ioutil"
	"net"
	"os"
	"os/signal"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"syscall"
	"time"

	"net/http"
	_ "net/http/pprof"

	"github.com/gotk3/gotk3/glib"
	"github.com/gotk3/gotk3/gtk"
	log "github.com/sirupsen/logrus"

	"github.com/awused/aw-man/internal/closing"
	"github.com/awused/aw-man/internal/config"
	"github.com/awused/aw-man/internal/gui"
	"github.com/awused/aw-man/internal/manager"
)

func main() {
	glib.SetPrgname("aw-man")
	gtk.Init(&[]string{"aw-man"})

	config.Load()

	if *config.DebugFlag {
		log.SetLevel(log.DebugLevel)
		go func() {
			log.Errorln(http.ListenAndServe("localhost:6060", nil))
		}()
	}

	if flag.NArg() != 1 {
		log.Fatalln("Provide exactly one archive, file, or directory to load")
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

	tmpDir, err := ioutil.TempDir(config.Conf.TempDirectory, "aw-man*")
	if err != nil {
		log.Fatalln(err)
	}
	defer os.RemoveAll(tmpDir)

	var sock net.Listener
	socketConns := make(chan net.Conn)
	if config.Conf.SocketDir != "" {
		sockPath := filepath.Join(
			config.Conf.SocketDir,
			"aw-man"+strconv.Itoa(os.Getpid())+".sock")
		sock, err = net.Listen("unix", sockPath)
		if err != nil {
			log.Panicln("Unable to create socket", sockPath, err)
		}
		defer sock.Close()

		go serveSocket(sock, socketConns)
	}

	wg := &sync.WaitGroup{}
	commandChan := make(chan manager.Command, 1)
	executableChan := make(chan string)
	sizeChan := make(chan image.Point)
	stateChan := make(chan manager.State)

	wg.Add(3)

	go manager.RunManager(
		commandChan,
		executableChan,
		sizeChan,
		stateChan,
		socketConns,
		tmpDir,
		wg,
		firstArchive)

	go func() {
		select {
		case <-sigs:
			closing.Close()
		case <-closing.Ch:
		}
		wg.Done()

		<-time.After(20 * time.Second)
		signal.Reset(syscall.SIGINT, syscall.SIGTERM)
		sock.Close()
		os.RemoveAll(tmpDir)
		if *config.DebugFlag {
			log.Errorln("Failed to exit in a timely manner:",
				"http://localhost:6060/debug/pprof/goroutine?debug=1")
		} else {
			log.Fatalln("Failed to exit in a timely manner")
		}
	}()

	gui.RunGui(
		commandChan,
		executableChan,
		sizeChan,
		stateChan,
		wg)
	wg.Wait()
}

// Very simple single threaded design, only deals with one connection at a time.
func serveSocket(sock net.Listener, ch chan<- net.Conn) {
	for {
		conn, err := sock.Accept()
		if err != nil {
			if !strings.Contains(err.Error(), "use of closed network connection") {
				log.Errorln("Socket accept error", err)
			}
			closing.Close()
			return
		}
		select {
		case ch <- conn:
		case <-closing.Ch:
			conn.Close()
			return
		}
	}
}
