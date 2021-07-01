package gui

import "C"
import (
	"image"
	"runtime/debug"
	"strconv"
	"sync"
	"time"

	"github.com/awused/aw-man/internal/closing"
	"github.com/awused/aw-man/internal/config"
	"github.com/awused/aw-man/internal/manager"
	"github.com/gotk3/gotk3/cairo"
	"github.com/gotk3/gotk3/gdk"
	"github.com/gotk3/gotk3/glib"
	"github.com/gotk3/gotk3/gtk"
	log "github.com/sirupsen/logrus"
)

var startTime time.Time = time.Now()
var commandTime time.Time = time.Now()

// Contains only the information to draw the GUI at this point in time
type gui struct {
	commandChan    chan<- manager.Command
	executableChan chan<- string
	stateChan      <-chan manager.State
	sizeChan       chan<- image.Point
	invalidChan    chan struct{}

	// Only accessed from main thread
	window          *gtk.Window
	state           manager.State
	surface         *cairo.Surface
	firstImagePaint bool
	imageChanged    bool
	hideUI          bool
	themeBG         bool
	isFullscreen    bool
	widgets         struct {
		canvas      *gtk.DrawingArea
		pageNumber  *gtk.Label
		archiveName *gtk.Label
		pageName    *gtk.Label
		bottomBar   *gtk.Box
	}

	// Guarded by l
	l               sync.Mutex
	commandQueue    []manager.Command
	executableQueue []string
	imageSize       image.Point
	prevImageSize   image.Point
}

func (g *gui) drawImage(da *gtk.DrawingArea, cr *cairo.Context) {
	cr.Save()
	defer cr.Restore()

	if !g.themeBG {
		cr.SetSourceRGBA(config.BG.R, config.BG.G, config.BG.B, config.BG.A)
		cr.SetOperator(cairo.OPERATOR_SOURCE)
		cr.Paint()
	} else {
		style, err := da.GetStyleContext()
		if err != nil {
			log.Panicln(err)
		}
		c, ok := style.LookupColor("unfocused_borders")
		if ok {
			cr.SetSourceRGBA(c.GetRed(), c.GetGreen(), c.GetBlue(), c.GetAlpha())
			cr.SetOperator(cairo.OPERATOR_SOURCE)
			cr.Paint()
		}
	}

	sz := image.Point{X: da.GetAllocatedWidth(), Y: da.GetAllocatedHeight()}
	g.l.Lock()
	imSz := g.imageSize
	if sz != imSz {
		g.imageSize = sz
		select {
		case g.sizeChan <- sz:
			commandTime = time.Now()
			g.prevImageSize = sz
		case g.invalidChan <- struct{}{}:
			// Go selects are performed in order.
		default:
		}
	}
	g.l.Unlock()

	img := g.state.Image
	if img == nil {
		return
	}

	if *config.DebugFlag && !g.firstImagePaint {
		log.Debugln("Time until first image paint started", time.Now().Sub(startTime))
	}

	if img.Bounds().Size().X == 0 || img.Bounds().Size().Y == 0 {
		log.Errorln("Tried to display 0 sized image", g.state)
		return
	}

	if g.imageChanged {
		if g.surface != nil {
			g.surface.Close()
		}

		var err error
		g.surface, err = cairo.CreateImageSurfaceForData(
			img.Pix,
			cairo.FORMAT_ARGB32,
			img.Bounds().Dx(),
			img.Bounds().Dy(),
			img.Stride)
		if err != nil {
			log.Errorln("Error creating surface for image", err)
			g.surface.Close()
			g.surface = nil
		}
	}

	if g.surface == nil {
		return
	}

	r := manager.CalculateImageBounds(g.state.OriginalBounds, sz)

	scale := 1.0
	if r.Size() != img.Bounds().Size() {
		log.Infoln(
			"Needed to scale at draw time", img.Bounds().Size(), "->", r.Size(), sz)
		scale = float64(r.Size().X) / float64(img.Bounds().Dx())
		cr.Scale(scale, scale)
	}
	cr.SetSourceSurface(g.surface, float64(r.Min.X)/scale, float64(r.Min.Y)/scale)
	cr.SetOperator(cairo.OPERATOR_OVER)
	cr.Paint()

	if !g.firstImagePaint {
		log.Infoln("Time until first image visible", time.Now().Sub(startTime))
	}

	if g.imageChanged && commandTime != (time.Time{}) {
		d := time.Now().Sub(commandTime)
		if d > 100*time.Millisecond {
			log.Infoln("Time from user action to image change", time.Now().Sub(commandTime))
		} else if d > 20*time.Millisecond {
			log.Debugln("Time from user action to image change", time.Now().Sub(commandTime))
		}
		if scale == 1.0 {
			commandTime = time.Time{}
		}
	}
	g.firstImagePaint = true
}

func (g *gui) layout() *gtk.Box {
	vbox, err := gtk.BoxNew(gtk.ORIENTATION_VERTICAL, 0)
	if err != nil {
		log.Panicln(err)
	}

	da, err := gtk.DrawingAreaNew()
	if err != nil {
		log.Panicln(err)
	}

	da.SetHAlign(gtk.ALIGN_FILL)
	da.SetVAlign(gtk.ALIGN_FILL)
	da.SetHExpand(true)
	da.SetVExpand(true)
	da.AddEvents(int(gdk.SCROLL_MASK))

	da.Connect("draw", g.drawImage)
	da.Connect("scroll-event", g.handleScroll)

	hbox, err := gtk.BoxNew(gtk.ORIENTATION_HORIZONTAL, 15)
	if err != nil {
		log.Panicln(err)
	}
	// TODO -- shrink, this is just for parity with Gio
	hbox.SetBorderWidth(10)
	hbox.SetMarginStart(30)
	hbox.SetMarginEnd(30)

	pageNum, err := gtk.LabelNew("")
	if err != nil {
		log.Panicln(err)
	}
	archiveName, err := gtk.LabelNew("")
	if err != nil {
		log.Panicln(err)
	}
	pageName, err := gtk.LabelNew("")
	if err != nil {
		log.Panicln(err)
	}

	hsep, err := gtk.LabelNew("|")
	if err != nil {
		log.Panicln(err)
	}
	hsep2, err := gtk.LabelNew("|")
	if err != nil {
		log.Panicln(err)
	}

	hbox.PackStart(pageNum, false, false, 0)
	hbox.PackStart(hsep, false, false, 0)
	hbox.PackStart(archiveName, false, false, 0)
	hbox.PackStart(hsep2, false, false, 0)
	hbox.PackStart(pageName, false, false, 0)

	vbox.PackStart(da, true, true, 0)
	vbox.PackEnd(hbox, false, false, 0)

	g.widgets.canvas = da
	g.widgets.pageNumber = pageNum
	g.widgets.archiveName = archiveName
	g.widgets.pageName = pageName
	g.widgets.bottomBar = hbox
	return vbox
}

func (g *gui) openWindow() {
	win, err := gtk.WindowNew(gtk.WINDOW_TOPLEVEL)
	if err != nil {
		log.Panicln("Unable to create window:", err)
	}
	win.SetTitle("aw-man")
	win.SetHideTitlebarWhenMaximized(true)
	win.Connect("destroy", func() {
		closing.Close()
		gtk.MainQuit()
	})
	go func() {
		<-closing.Ch
		gtk.MainQuit()
	}()

	// Set the default window size.
	win.SetDefaultSize(800, 600)

	// Enable RGBA if available
	s := win.GetScreen()
	v, err := s.GetRGBAVisual()
	if v != nil && err == nil {
		win.SetVisual(v)
	} else {
		log.Warningln("Unable to create RGBA window", err)
	}

	win.AddEvents(int(gdk.KEY_PRESS_MASK))

	win.Connect("key-press-event", g.handleKeyPress)
	win.Connect("window-state-event", func(win *gtk.Window, ev *gdk.Event) {
		e := gdk.EventWindowStateNewFromEvent(ev)
		g.isFullscreen = (e.NewWindowState() & gdk.WINDOW_STATE_FULLSCREEN) != 0
	})

	win.Add(g.layout())
	g.window = win
}

func (g *gui) handleState(gs manager.State) {
	if g.state.Image != gs.Image {
		g.imageChanged = true
		g.widgets.canvas.QueueDraw()
	}

	if g.state.ArchiveName != gs.ArchiveName {
		g.window.SetTitle(gs.ArchiveName + " - aw-man")
	}

	g.widgets.pageNumber.SetLabel(strconv.Itoa(gs.PageNumber) + " / " + strconv.Itoa(gs.ArchiveLength))
	g.widgets.archiveName.SetLabel(gs.ArchiveName)
	g.widgets.pageName.SetLabel(gs.PageName)

	g.state = gs

}

func (g *gui) loop(wg *sync.WaitGroup) {
	defer wg.Done()

	for {
		var sizeCh chan<- image.Point
		var cmdCh chan<- manager.Command
		var execCh chan<- string

		var cmdToSend manager.Command
		var execToSend string

		g.l.Lock()
		sz := g.imageSize
		prevSz := g.prevImageSize
		if len(g.commandQueue) > 0 {
			cmdToSend = g.commandQueue[0]
			cmdCh = g.commandChan
		}
		if len(g.executableQueue) > 0 {
			execToSend = g.executableQueue[0]
			execCh = g.executableChan
		}
		g.l.Unlock()

		if sz != prevSz {
			sizeCh = g.sizeChan
		}

		select {
		case <-closing.Ch:
			g.window.Close()
			return
		case cmdCh <- cmdToSend:
			commandTime = time.Now()
			g.l.Lock()
			g.commandQueue = g.commandQueue[1:]
			g.l.Unlock()
		case execCh <- execToSend:
			g.l.Lock()
			g.executableQueue = g.executableQueue[1:]
			g.l.Unlock()
		case gs := <-g.stateChan:
			glib.IdleAdd(func() { g.handleState(gs) })
		case sizeCh <- sz:
			commandTime = time.Now()
			g.l.Lock()
			g.prevImageSize = sz
			g.l.Unlock()
		case <-g.invalidChan:
		}
	}
}

func RunGui(
	commandChan chan<- manager.Command,
	executableChan chan<- string,
	sizeChan chan<- image.Point,
	stateChan <-chan manager.State,
	wg *sync.WaitGroup) {
	g := gui{
		commandChan:    commandChan,
		executableChan: executableChan,
		sizeChan:       sizeChan,
		stateChan:      stateChan,
		invalidChan:    make(chan struct{}, 1),
	}
	g.run(wg)
}

func (g *gui) run(
	wg *sync.WaitGroup) {
	defer wg.Done()
	defer func() {
		if r := recover(); r != nil {
			log.Errorln("Gui panic: \n", r.(error), "\n", string(debug.Stack()))
			closing.Close()
		}
	}()

	g.surface, _ = cairo.NewSurfaceFromPNG("/cache/temp/temp/out.png")

	g.openWindow()

	parseShortcuts()

	wg.Add(1)
	go g.loop(wg)

	// Recursively show all widgets contained in this window.
	g.window.ShowAll()

	// Begin executing the GTK main loop.  This blocks until
	// gtk.MainQuit() is run.
	gtk.Main()
}
