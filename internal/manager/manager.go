package manager

import (
	"image"
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

	// Only set when the manager is waiting for the UI to be done rendering.
	Waiting chan<- struct{}
}

// The archive and page indices for a given page
type pageIndices struct {
	a int
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
}

func (m *manager) get(pi pageIndices) (*archive, *page, *loadableImage) {
	if len(m.archives) < pi.a {
		log.Panicf("Tried to get %v but archive does not exist\n", pi)
	}
	a := m.archives[pi.a]
	p, li := a.Get(pi.p, m.upscaling)
	return a, p, li
}

// func (m *manager) dist(a pageIndices, b pageIndices) int {
// 	if a.gt(b) {
// 		a, b = b, a
// 	}
//
// 	d := 0
// 	for a.a != b.a {
// 		if len(m.archives[a.a].pages) == 0 {
// 			// Treat an empty/invalid archive as containing one image
// 			d++
// 		} else {
// 			d += len(m.archives[a.a].pages) - a.p
// 		}
// 		a.a++
// 		a.p = 0
// 	}
// 	d += b.p - a.p
//
// 	return d
// }

func (m *manager) add(pi pageIndices, x int) (pageIndices, bool) {
	pi.p += x
	for pi.a < len(m.archives)-1 && pi.p >= m.archives[pi.a].PageCount() && pi.p > 0 {
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
		} else if c, u := cp.CanLoad(m.upscaling); (c && !u) || cli.IsLoading() {
			// Keep the old image visible rather than showing a blank screen if we're only waiting on a
			// load.
			s.Image, s.OriginalBounds = m.s.Image, m.s.OriginalBounds
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
	wait := make(chan struct{})
	m.s.Waiting = wait
	select {
	case m.stateChan <- m.s:
	case <-closing.Ch:
	}
	m.s.Waiting = nil

	// select {
	// case <-wait:
	// case <-closing.Ch:
	// }
}

// Unload all the images and dispose of any archives that are unnecessary now.
func (m *manager) afterMove(oldc pageIndices) {
	_, _, cli := m.get(m.c)

	if cli != nil && cli.IsLoading() {
		select {
		case msi := <-cli.loadCh:
			cli.MarkLoaded(msi)
			m.updateState()
		default:
		}
	}

	m.nl = m.c
	m.nu = m.c
	m.findNextImageToLoad()

	// Start at the old lower limit of loading and /advance/ to new lower limit
	// Start at old upper limit and /reverse/ to new upper limit
	if newStart, ok := m.add(m.c, -config.Conf.Retain); ok {
		pi, ok := m.add(oldc, -config.Conf.Retain)
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

	if newEnd, ok := m.add(m.c, config.Conf.Preload); ok {
		pi, ok := m.add(oldc, config.Conf.Preload)
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

	// When cleaning up archives, be sure to adjust indices
	m.updateState()
}

func (m *manager) canLoad(pi pageIndices) bool {
	_, p, _ := m.get(pi)
	if p == nil {
		return false
	}

	// can, ups := p.CanLoad(m.upscaling)
	// return can && (!ups || pi.a >= m.c.a)
	can, _ := p.CanLoad(m.upscaling)
	return can
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
	lastPreload, _ := m.add(m.c, config.Conf.Preload)
	if !m.c.gt(m.nl) {
		for !m.nl.gt(lastPreload) {
			if m.canLoad(m.nl) {
				return
			}
			if nl, ok := m.add(m.nl, 1); ok {
				if nl.a != m.c.a && !m.mangaMode {
					// Don't start loading the next archive into memory.
					break
				}
				m.nl = nl
			} else {
				break
			}
		}
		if nl, ok := m.add(m.c, -1); ok {
			m.nl = nl
		} else {
			m.nl = m.c
		}
	}

	firstPreload, _ := m.add(m.c, -config.Conf.Retain)

	for !firstPreload.gt(m.nl) {
		if m.canLoad(m.nl) {
			return
		}
		if nl, ok := m.add(m.nl, -1); ok {
			if nl.a != m.c.a && !m.mangaMode /* && !allowPreviousArchive */ {
				// Don't start loading the previous archive into memory.
				// TODO -- never upscale a previous archive
				break
			}
			m.nl = nl
		} else {
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
	firstArchive string) {
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
	var resizeDebounce <-chan time.Time
	loadingSem = make(chan struct{}, *&config.Conf.LoadThreads)
	ct := map[Command]func(){
		NextPage:  m.nextPage,
		PrevPage:  m.prevPage,
		FirstPage: m.firstPage,
		LastPage:  m.lastPage,
	}

	a, p := openArchive(
		firstArchive, m.targetSize, m.tmpDir, waitingOnFirst, false)
	m.archives, m.c.p = []*archive{a}, p
	m.findNextImageToLoad()

	lastSentState := m.s
	m.updateState()
	m.blockingSendState()

	for {
		var extractionCh <-chan bool
		var upscaleCh <-chan bool
		var loadCh <-chan maybeScaledImage
		var upscaleExtractionCh <-chan bool
		var upscaleJobsCh chan<- struct{}
		var stateCh chan<- State

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

		// Prioritize the current load over future loads
		if loadCh == nil &&
			nlli != nil && nlli.ReadyToLoad() && m.targetSize != (image.Point{}) {
			nlli.Load(m.targetSize)
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

		if m.s != lastSentState {
			stateCh = m.stateChan
		}

		select {
		case <-closing.Ch:
			return
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
		case msi := <-loadCh:
			cli.MarkLoaded(msi)
			m.updateState()
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
		case stateCh <- m.s:
			lastSentState = m.s
		case m.targetSize = <-m.sizeChan:
			m.invalidateStaleDownscaledImages()
			m.nl = m.c
			if cli != nil && resizeDebounce == nil {
				cli.maybeRescale(m.targetSize)
				if cli.ReadyToLoad() {
					img := cli.loadSync(m.targetSize, true)
					m.updateState()
					m.blockingSendState()
					cli.Rescale(m.targetSize, img)
				}
				m.updateState()
			}
			resizeDebounce = time.After(1000 * time.Millisecond)
		case <-resizeDebounce:
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
		// if !*mangaMode && nc.a != m.c.a { return }
		oldc := m.c
		m.c = nc
		m.afterMove(oldc)
	}
}

func (m *manager) prevPage() {
	if nc, ok := m.add(m.c, -1); ok {
		// if !*mangaMode && nc.a != m.c.a { return }
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