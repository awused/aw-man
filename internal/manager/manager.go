package manager

import (
	"image"
	"image/draw"
	"net"
	"path/filepath"
	"runtime/debug"
	"sync"
	"time"

	"github.com/awused/aw-man/internal/closing"
	"github.com/awused/aw-man/internal/config"
	log "github.com/sirupsen/logrus"
)

// Command represents user input.
type Command int8

// Commands represent user input.
const (
	NextPage Command = iota
	PrevPage
	LastPage
	FirstPage
	NextArchive
	PrevArchive
	UpscaleToggle
	//UpscaleLockToggle
	MangaToggle
	Jump
)

// UserCommand represents user input with arguments.
type UserCommand struct {
	Cmd Command
	Arg string
	Ch  chan<- error // nullable
}

// SocketCommand represents a command from the socket IPC API.
// These need to be sent to the GUI thread for easier parsing and routing.
type SocketCommand struct {
	Cmd string
	Ch  chan<- error
}

// Executable represents an action that doesn't match an internal command.
// We attempt to run it as an executable with no arguments.
type Executable struct {
	Exec string
	Ch   chan<- error // nullable
}

// State is a snapshot of the program's state for re-rendering the UI.
type State struct {
	Image          *BGRA
	OriginalBounds image.Rectangle
	PageNumber     int
	PageName       string
	ArchiveLength  int
	ArchiveName    string
	Upscaling      bool
	MangaMode      bool
	//UpscaleLock bool
}

type manager struct {
	tmpDir         string
	wg             *sync.WaitGroup
	archives       []*archive
	commandChan    <-chan UserCommand
	executableChan <-chan Executable
	stateChan      chan<- State
	socketConns    <-chan net.Conn
	socketCommands []SocketCommand
	socketCmdChan  chan<- SocketCommand
	upscaling      bool
	mangaMode      bool
	//alwaysUpscale  bool // Upscale files even if currently displaying unscaled
	// The "c"urrently displayed image
	c pageIndices
	// The "n"ext image to "u"pscale (waiting on extraction)
	nu pageIndices
	// The "n"ext image to "l"oad (waiting on extraction or upscale)
	nl pageIndices

	sizeChan   <-chan image.Point
	targetSize image.Point
	s          State

	// For the directory fast path
	firstImageFromFile *BGRA
}

func (m *manager) join() {
	for _, a := range m.archives {
		a.Close(m.wg)
	}
}

func (m *manager) updateState() {
	ca, cp, cli := m.get(m.c)
	s := State{
		ArchiveLength: ca.PageCount(),
		ArchiveName:   ca.name,
		Upscaling:     m.upscaling,
		MangaMode:     m.mangaMode,
		// Loading: cli != nil && cli.IsLoading()
	}

	if cp != nil {
		s.PageName = cp.name
		s.PageNumber = m.c.p + 1
	}

	if cli != nil {
		if cli.HasImageData() {
			s.Image = cli.msi.img
			// If loaded and no image display error
			s.OriginalBounds = cli.msi.originalBounds
			m.firstImageFromFile = nil
		} else if c, u := cp.CanLoad(m.upscaling); (c && !u) || cli.IsLoading() {
			// Keep the old image visible rather than showing a blank screen if we're only waiting on a
			// load.
			s.Image, s.OriginalBounds = m.s.Image, m.s.OriginalBounds
		} else if m.firstImageFromFile != nil {
			s.Image = m.firstImageFromFile
			s.OriginalBounds = m.firstImageFromFile.Bounds()
		} else {
			// On failure, do something
		}
	} else {
		// Empty archive, display error
	}

	// If empty archive display error
	m.s = s
}

// Send the state to the GUI and wait for it to finish rendering to try to avoid CPU contention.
func (m *manager) blockingSendState() {
	select {
	case m.stateChan <- m.s:
	case <-closing.Ch:
	}
}

func (m *manager) closeUnusedArchives(newStart, newEnd, firstUpscaled, lastUpscaled pageIndices) {
	for {
		ac := archiveIndex(len(m.archives) - 1)
		if m.c.a < ac && newEnd.a < ac && lastUpscaled.a < ac {
			a := m.archives[ac]
			m.archives = m.archives[:ac]
			a.Close(m.wg)
		} else {
			break
		}
	}

	for m.c.a > 0 && newStart.a > 0 && firstUpscaled.a > 0 {
		a := m.archives[0]
		m.archives = m.archives[1:]
		m.c.a--
		m.nl.a--
		m.nu.a--
		newStart.a--
		firstUpscaled.a--
		a.Close(m.wg)
	}
}

// Advances m.nl to the next image that should be loaded, if one can be found
func (m *manager) findNextImageToLoad() {
	// TODO -- use most of this same code for upscaling
	lastPreload, _ := m.add(m.c, config.Conf.PreloadAhead)
	if !m.c.gt(m.nl) {
		for !m.nl.gt(lastPreload) {
			_, p, _ := m.get(m.nl)
			if p != nil {
				if can, _ := p.CanLoad(m.upscaling); can {
					return
				}
			}
			if nl, ok := m.add(m.nl, 1); ok {
				if nl.a != m.c.a && !m.mangaMode {
					// Don't start loading the next archive into memory.
					break
				}
				m.nl = nl
			} else {
				// TODO -- Make opening next/previous archives asynchronous.
				if m.mangaMode && m.openNextArchive(preloading) != nil {
					// Must figure out the new last image to preload.
					lastPreload, _ = m.add(m.c, config.Conf.PreloadAhead)
					continue
				} else {
					break
				}
			}
		}
		m.nl = m.c
	}

	firstPreload, _ := m.add(m.c, -config.Conf.PreloadBehind)

	for !firstPreload.gt(m.nl) {
		_, p, _ := m.get(m.nl)
		if p != nil {
			if can, _ := p.CanLoad(m.upscaling); can {
				return
			}
		}
		if nl, ok := m.add(m.nl, -1); ok {
			if nl.a != m.c.a && !m.mangaMode && (!m.upscaling || !config.Conf.UpscalePreviousChapters) {
				// Don't start loading the previous archive into memory.
				break
			}
			m.nl = nl
		} else {
			// TODO -- Make opening next/previous archives asynchronous.
			if m.mangaMode && m.openPreviousArchive(preloading) != nil {
				// Must figure out the new first image to preload.
				firstPreload, _ = m.add(m.c, -config.Conf.PreloadBehind)
				continue
			} else {
				break
			}
		}
	}
	// Just park it on the current page
	m.nl = m.c
}

func (m *manager) findNextImageToUpscale() {
}

func (m *manager) invalidateStaleDownscaledImages() {
	for _, a := range m.archives {
		for _, p := range a.pages {
			p.invalidateDownscaled(m.targetSize)
		}
	}
}
func (m *manager) maybeRescaleLargerImages() {
	for _, a := range m.archives {
		for _, p := range a.pages {
			p.maybeRescale(m.targetSize)
		}
	}
}

func (m *manager) openNextArchive(ot openType) *archive {
	if a := m.archives[len(m.archives)-1]; a.kind != directory {
		fname, dir := filepath.Base(a.path), filepath.Dir(a.path)
		_, after := findBeforeAndAfterInDir(fname, dir)
		if after == "" {
			return nil
		}
		na, _ := openArchive(filepath.Join(dir, after), m.tmpDir, ot, m.upscaling)
		m.archives = append(m.archives, na)
		return na
	}
	return nil
}

func (m *manager) openPreviousArchive(ot openType) *archive {
	if a := m.archives[0]; a.kind != directory {
		fname, dir := filepath.Base(a.path), filepath.Dir(a.path)
		before, _ := findBeforeAndAfterInDir(fname, dir)
		if before == "" {
			return nil
		}
		na, _ := openArchive(filepath.Join(dir, before), m.tmpDir, ot, m.upscaling)
		// No need to get fancy here.
		m.archives = append([]*archive{na}, m.archives...)
		m.c.a++
		m.nl.a++
		m.nu.a++
		return na
	}
	return nil
}

// RunManager starts the manager, which is responsible for managing all the
// resources (archives, images), jobs (extractions, upscales, and
// loads/unloads), and responding to user input from the GUI.
func RunManager(
	commandChan <-chan UserCommand,
	executableChan <-chan Executable,
	sizeChan <-chan image.Point,
	stateChan chan<- State,
	socketConns <-chan net.Conn,
	socketCmdChan chan<- SocketCommand,
	tmpDir string,
	wg *sync.WaitGroup,
	firstArchive string) {
	(&manager{
		tmpDir:         tmpDir,
		wg:             wg,
		commandChan:    commandChan,
		executableChan: executableChan,
		sizeChan:       sizeChan,
		stateChan:      stateChan,
		socketConns:    socketConns,
		socketCmdChan:  socketCmdChan,
		mangaMode:      config.MangaMode,
	}).run(firstArchive)
}

func (m *manager) run(
	initialFile string) {
	defer m.wg.Done()
	defer m.join()
	defer func() {
		if r := recover(); r != nil {
			log.Errorln("Manager panic: \n", r, "\n", string(debug.Stack()))
			closing.Close()
		}
	}()
	// We don't want to try to resize immediately if the window is being resized
	// rapidly
	resizeDebounce := time.NewTimer(time.Second)
	resizeDebounce.Stop()

	loadingSem = make(chan struct{}, *&config.Conf.LoadThreads)
	conversionSem = make(chan struct{}, *&config.Conf.LoadThreads)
	simpleCommands := map[Command]func(){
		NextPage:    m.nextPage,
		PrevPage:    m.prevPage,
		FirstPage:   m.firstPage,
		LastPage:    m.lastPage,
		NextArchive: m.nextArchive,
		PrevArchive: m.prevArchive,
		MangaToggle: m.mangaToggle,
	}
	argCommands := map[Command]func(string) error{
		Jump: m.jump,
	}

	if isNativelySupportedImage(initialFile) {
		// Fast path to load a single image.
		// Relevant for very large directories, or those on remote file systems.
		img := loadImageFromFile(initialFile)
		nimg := image.NewRGBA(img.Bounds())
		draw.Draw(nimg, nimg.Bounds(), img, img.Bounds().Min, draw.Src)
		m.firstImageFromFile = fromRGBA(nimg)
		m.s.Image = m.firstImageFromFile
		m.s.OriginalBounds = m.firstImageFromFile.Bounds()
		m.blockingSendState()
	}

	a, p := openArchive(initialFile, m.tmpDir, waitingOnFirst, false)
	m.archives, m.c.p = []*archive{a}, p
	m.nl = m.c
	m.nu = m.c
	m.findNextImageToLoad()

	m.updateState()
	m.blockingSendState()
	lastSentState := m.s

	for {
		var extractionCh <-chan bool
		var upscaleCh <-chan bool
		var loadCh <-chan maybeScaledImage
		var upscaleExtractionCh <-chan bool
		var upscaleJobsCh chan<- struct{}
		var socketCmdCh chan<- SocketCommand
		var stateCh chan<- State

		var socketCmd = SocketCommand{}

		_, cp, cli := m.get(m.c)

		// Assertions
		// If we're waiting for anything on the current page, that should take
		// priority. If it's not then we've screwed up.
		if cp != nil {
			if cp.state == extracting && m.c != m.nl {
				log.Panicf(
					"Current page %v %s is not extracted but next page to "+
						"load is %v", m.c, cp, m.nl)
			}
			if m.upscaling {
				if cp.state != upscaled && m.c != m.nl {
					log.Panicf(
						"Current page %v %s is not upscaled but next page to "+
							"load is %v", m.c, cp, m.nl)
				}
			}

			if cli != nil && cli.state <= loadable && m.c != m.nl {
				log.Panicf(
					"Current image %v %s %s is not loaded but next image to "+
						"load is %v", m.c, cp, cli, m.nl)
			}
		}

		if cli != nil && cli.state == loading {
			loadCh = cli.loadCh
		}

		// Determine if we need to wait on anything for the next image we want to
		// load.
		_, nlp, nlli := m.get(m.nl)
		// nlp is only nil in the case of empty archives
		if nlp != nil {
			if nlp.state == extracting {
				extractionCh = nlp.extractCh
			}
			if m.upscaling {
				if nlp.state == upscaling {
					upscaleCh = nlp.upscaleCh
				}
				if nlp.CanUpscale() {
					// The next image we want to load hasn't even started upscaling
					m.nu = m.nl
				}
			}
		}

		// Try not to start new loads if the UI is blocked.
		// TODO -- reconsider this
		if (loadCh == nil || m.c.p != 0) &&
			stateCh == nil &&
			nlli != nil &&
			nlli.ReadyToLoad() &&
			(m.targetSize != (image.Point{}) || m.c == m.nl) {
			sz := m.targetSize
			if /*m.c.p == 0 && */ m.c == m.nl {
				// If we're blocking the UI on showing an image, scale it using a cheap method first.
				// The CatmullRom scaler can take hundreds of milliseconds.
				// It can be rescaled properly later.
				sz = image.Point{}
			}
			nlli.Load(sz)
			m.findNextImageToLoad()
			continue
		}

		_, nup, _ := m.get(m.nu)
		if m.upscaling && nup != nil {
			// if nup.ReadyToUpscale() {
			// } else if nup.state == extracting {
			//
			// }
		}

		if m.s != lastSentState {
			stateCh = m.stateChan
		}

		if len(m.socketCommands) > 0 {
			socketCmd = m.socketCommands[0]
			socketCmdCh = m.socketCmdChan
		}

		select {
		case <-closing.Ch:
			return
		case stateCh <- m.s:
			lastSentState = m.s
		case msi := <-loadCh:
			cli.MarkLoaded(msi)
			m.updateState()
			// TODO -- We only start rescaling once the image is displayed, which could be better.
			cli.maybeRescale(m.targetSize)
		case s := <-extractionCh:
			nlp.MarkExtracted(s)
			// if m.upscaling && m.nl == m.nu && !nlp.ReadyToUpscale() {
			//   m.findNextImageToUpscale()
			// }
		case s := <-upscaleCh:
			nlp.MarkUpscaled(s)
		case s := <-upscaleExtractionCh:
			nup.MarkExtracted(s)
			// if !nup.ReadyToUpscale() {
			//   m.findNextImageToUpscale()
			// }
		case upscaleJobsCh <- struct{}{}:
			nup.state = upscaling
			//m.findNextImageToUpscale()
		case uc := <-m.commandChan:
		InputLoop:
			for {
				if f, ok := simpleCommands[uc.Cmd]; ok {
					f()
				} else if f, ok := argCommands[uc.Cmd]; ok {
					err := f(uc.Arg)
					if err != nil && uc.Ch != nil {
						// The other end will be waiting, but be safe.
						select {
						case uc.Ch <- err:
						case <-closing.Ch:
						}
					}
				} else {
					// Should never happen
					log.Panicln("Received internal command", uc.Cmd, "with no handler")
				}
				if uc.Ch != nil {
					close(uc.Ch)
				}
				// Consume more input, if available, to make a best-effort attempt at satisfying the UI as
				// fast as possible.
				select {
				case uc = <-m.commandChan:
				default:
					break InputLoop
				}
			}
			m.updateState()
		case m.targetSize = <-m.sizeChan:
			m.invalidateStaleDownscaledImages()
			m.nl = m.c
			if cli != nil && resizeDebounce == nil {
				cli.maybeRescale(m.targetSize)
				if cli.ReadyToLoad() {
					cli.loadSyncUnscaled()
					m.updateState()
					m.blockingSendState()
					cli.maybeRescale(m.targetSize)
				}
				m.updateState()
			}
			if !resizeDebounce.Stop() {
				select {
				case <-resizeDebounce.C:
				default:
				}
			}
			resizeDebounce.Reset(500 * time.Millisecond)
		case <-resizeDebounce.C:
			m.invalidateStaleDownscaledImages()
			m.maybeRescaleLargerImages()
			m.nl = m.c
			m.findNextImageToLoad()
		case c := <-m.socketConns:
			m.handleConn(c)
		case e := <-m.executableChan:
			m.runExecutable(e)
		case socketCmdCh <- socketCmd:
			m.socketCommands = m.socketCommands[1:]
		}
	}
}
