package manager

import (
	"image"
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
)

// State is a snapshot of the program's state for re-rendering the UI.
type State struct {
	Image          image.Image
	OriginalBounds image.Rectangle
	PageNumber     int
	PageName       string
	ArchiveLength  int
	ArchivePath    string
	Upscaling      bool
	MangaMode      bool
	//UpscaleLock bool
}

// Type safety so archive indices and page indices don't get mixed up.
type archiveIndex int

// The archive and page indices for a given page
type pageIndices struct {
	a archiveIndex
	p int
}

func (a pageIndices) gt(b pageIndices) bool {
	if a.a != b.a {
		return a.a > b.a
	}
	return a.p > b.p
}

type manager struct {
	tmpDir      string
	wg          *sync.WaitGroup
	archives    []*archive
	commandChan <-chan Command
	stateChan   chan<- State
	upscaling   bool
	mangaMode   bool
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
	firstImageFromFile image.Image
}

func (m *manager) get(pi pageIndices) (*archive, *page, *loadableImage) {
	if len(m.archives) < int(pi.a) {
		log.Panicf("Tried to get %v but archive does not exist\n", pi)
	}
	a := m.archives[pi.a]
	p, li := a.Get(pi.p, m.upscaling)
	return a, p, li
}

// add translates pi by x pages, and returns whether or not that represents a page from the
// opened archives.
func (m *manager) add(pi pageIndices, x int) (pageIndices, bool) {
	pi.p += x
	for int(pi.a) < len(m.archives)-1 && pi.p >= m.archives[pi.a].PageCount() && pi.p > 0 {
		a := m.archives[pi.a]
		if a.PageCount() == 0 {
			pi.p--
		}
		pi.p -= a.PageCount()
		pi.a++
	}

	for pi.a > 0 && pi.p < 0 {
		pi.a--
		a := m.archives[pi.a]
		if a.PageCount() == 0 {
			pi.p++
		}
		pi.p += a.PageCount()
	}

	return pi, pi.p >= 0 && (pi.p == 0 || pi.p < m.archives[pi.a].PageCount())
}

func (m *manager) join() {
	for _, a := range m.archives {
		a.Close(m.wg)
	}
}

func (m *manager) updateState() {
	ca, cp, cli := m.get(m.c)
	s := State{
		PageNumber:    m.c.p,
		ArchiveLength: ca.PageCount(),
		ArchivePath:   ca.path,
		Upscaling:     m.upscaling,
		MangaMode:     m.mangaMode,
		// Loading: cli != nil && cli.IsLoading()
	}

	if cp != nil {
		s.PageName = cp.name
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

// Send the state to the GUI and wait for it to finish rendering to avoid CPU contention.
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

// Unload all the images and dispose of any archives that are unnecessary now.
func (m *manager) afterMove(oldc pageIndices) {
	_, _, cli := m.get(m.c)

	if cli != nil && cli.IsLoading() {
		select {
		case msi := <-cli.loadCh:
			cli.MarkLoaded(msi)
			m.updateState()
			cli.maybeRescale(m.targetSize)
		default:
		}
	}

	m.nl = m.c
	m.nu = m.c
	m.findNextImageToLoad()
	// TODO -- next image to upscale

	// Start at the old lower limit of loading and /advance/ to new lower limit
	newStart, ok := m.add(m.c, -config.Conf.PreloadBehind)
	if ok {
		pi, ok := m.add(oldc, -config.Conf.PreloadBehind)
		for newStart.gt(pi) {
			if ok {
				_, p, _ := m.get(pi)
				if p != nil {
					p.unload()
				}
			}
			pi, ok = m.add(pi, 1)
		}
	}

	// Start at old upper limit and /reverse/ to new upper limit
	newEnd, ok := m.add(m.c, config.Conf.PreloadAhead)
	if ok {
		pi, ok := m.add(oldc, config.Conf.PreloadAhead)
		for pi.gt(newEnd) {
			if ok {
				_, p, _ := m.get(pi)
				if p != nil {
					p.unload()
				}
			}
			pi, ok = m.add(pi, -1)
		}
	}

	// TODO -- Now clean up upscales
	lastUpscaled, firstUpscaled := m.c, m.c

	// Removing archives off the end first to avoid updating more than we need those
	m.closeUnusedArchives(newStart, newEnd, firstUpscaled, lastUpscaled)

	// When cleaning up archives, be sure to adjust indices
	m.firstImageFromFile = nil
	m.updateState()
}

func (m *manager) canUpscale(pi pageIndices) bool {
	if !m.upscaling {
		return false
	}
	return false
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
				// TODO -- load next chapter
				break
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
			// TODO -- load previous chapter when relevant
			break
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
		na, _ := openArchive(filepath.Join(dir, after), m.targetSize, m.tmpDir, ot, m.upscaling)
		m.archives = append(m.archives, na)
		log.Debugln("Opened next archive", m.archives)
		return na
	}
	return nil
}

func (m *manager) openPreviousArchive(ot openType) *archive {
	if a := m.archives[m.c.a]; a.kind != directory {
		fname, dir := filepath.Base(a.path), filepath.Dir(a.path)
		before, _ := findBeforeAndAfterInDir(fname, dir)
		if before == "" {
			return nil
		}
		na, _ := openArchive(filepath.Join(dir, before), m.targetSize, m.tmpDir, ot, m.upscaling)
		// No need to get fancy here.
		m.archives = append([]*archive{na}, m.archives...)
		log.Debugln("Opened previous archive", m.archives)
		m.c.a++
		return na
	}
	return nil
}

// RunManager starts the manager, which is responsible for managing all the
// resources (archives, images), jobs (extractions, upscales, and
// loads/unloads), and responding to user input from the GUI.
func RunManager(
	commandChan <-chan Command,
	sizeChan <-chan image.Point,
	stateChan chan<- State,
	tmpDir string,
	wg *sync.WaitGroup,
	firstArchive string) {
	(&manager{
		tmpDir:      tmpDir,
		wg:          wg,
		commandChan: commandChan,
		sizeChan:    sizeChan,
		stateChan:   stateChan,
	}).run(firstArchive)
}

func (m *manager) run(
	initialFile string) {
	defer m.wg.Done()
	defer m.join()
	defer func() {
		if r := recover(); r != nil {
			log.Errorln("Manager panic: \n" + string(debug.Stack()))
			closing.Once()
		}
	}()
	// We don't want to try to resize immediately if the window is being resized
	// rapidly
	var resizeDebounce *time.Timer
	loadingSem = make(chan struct{}, *&config.Conf.LoadThreads)
	ct := map[Command]func(){
		NextPage:    m.nextPage,
		PrevPage:    m.prevPage,
		FirstPage:   m.firstPage,
		LastPage:    m.lastPage,
		NextArchive: m.nextArchive,
		PrevArchive: m.prevArchive,
	}

	if isNativelySupportedImage(initialFile) {
		// Fast path to load a single image.
		// Relevant for very large directories, or those on remote file systems.
		m.firstImageFromFile = loadImageFromFile(initialFile)
		m.s.Image = m.firstImageFromFile
		m.s.OriginalBounds = m.firstImageFromFile.Bounds()
		m.blockingSendState()
	}

	a, p := openArchive(
		initialFile, m.targetSize, m.tmpDir, waitingOnFirst, false)
	m.archives, m.c.p = []*archive{a}, p
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
		var stateCh chan<- State
		var timerCh <-chan time.Time

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
		// load
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

		if m.s != lastSentState {
			stateCh = m.stateChan
		}

		// Try not to start new loads if the UI is blocked.
		if (loadCh == nil || m.c.p != 0) &&
			stateCh == nil &&
			nlli != nil &&
			nlli.ReadyToLoad() &&
			(m.targetSize != (image.Point{}) || m.c == m.nl) {
			sz := m.targetSize
			if m.c.p == 0 && m.c == m.nl {
				sz = image.Point{}
			}
			nlli.Load(sz)
			m.findNextImageToLoad()
			continue
		}

		_, nup, _ := m.get(m.nu)
		if m.upscaling && nup != nil {
			if nup.CanUpscale() {

			} else {
				m.findNextImageToUpscale()
				// Advance to next image
				continue
			}
		}

		if resizeDebounce != nil {
			timerCh = resizeDebounce.C
		}

		select {
		case <-closing.Ch:
			return
		case stateCh <- m.s:
			lastSentState = m.s
		case msi := <-loadCh:
			cli.MarkLoaded(msi)
			m.updateState()
			cli.maybeRescale(m.targetSize)
		case s := <-extractionCh:
			nlp.MarkExtracted(s)
			/*if c, u := nlp.CanLoad(m.upscaling); c && !u {
				nlp.normal.load(m.targetSize)
			}
			m.findNextImageToLoad()*/
		case s := <-upscaleCh:
			nlp.MarkUpscaled(s)
			/*if c, _ := nlp.CanLoad(m.upscaling); c {
				nlp.upscale.load(m.targetSize)
			}
			m.findNextImageToLoad()*/
		case s := <-upscaleExtractionCh:
			nup.MarkExtracted(s)
			// m.findNextImageToUpscale
		case upscaleJobsCh <- struct{}{}:
			nup.state = upscaling
			// TODO -- Advance to next image
			// m.findNextImageToUpscale
		case c := <-m.commandChan:
			if f, ok := ct[c]; ok {
				f()
			}
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
			if resizeDebounce != nil {
				resizeDebounce.Stop()
			}
			resizeDebounce = time.NewTimer(500 * time.Millisecond)
		case <-timerCh:
			m.invalidateStaleDownscaledImages()
			m.maybeRescaleLargerImages()
			m.nl = m.c
			m.findNextImageToLoad()
			resizeDebounce = nil
		}
	}
}

func (m *manager) nextPage() {
	if nc, ok := m.add(m.c, 1); ok {
		if !m.mangaMode && nc.a != m.c.a {
			return
		}
		oldc := m.c
		m.c = nc
		m.afterMove(oldc)
	}
}

func (m *manager) prevPage() {
	if nc, ok := m.add(m.c, -1); ok {
		if !m.mangaMode && nc.a != m.c.a {
			return
		}
		oldc := m.c
		m.c = nc
		m.afterMove(oldc)
	}
}

func (m *manager) firstPage() {
	oldc := m.c
	m.c.p = 0
	if oldc != m.c {
		m.afterMove(oldc)
	}
}

func (m *manager) lastPage() {
	oldc := m.c
	m.c.p = m.archives[m.c.a].PageCount() - 1
	if m.c.p < 0 {
		m.c.p = 0
	}
	if oldc != m.c {
		m.afterMove(oldc)
	}
}

func (m *manager) nextArchive() {
	if int(m.c.a) == len(m.archives)-1 {
		if m.openNextArchive(waitingOnFirst) == nil {
			return
		}
	}

	oldc := m.c
	m.c.a, m.c.p = m.c.a+1, 0
	m.afterMove(oldc)
}

func (m *manager) prevArchive() {
	if int(m.c.a) == 0 {
		if m.openPreviousArchive(waitingOnFirst) == nil {
			return
		}
	}

	oldc := m.c
	m.c.a, m.c.p = m.c.a-1, 0
	m.afterMove(oldc)
}
