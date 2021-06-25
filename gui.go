package main

import (
	"image"
	"image/color"
	"runtime/debug"
	"strconv"
	"strings"
	"sync"
	"time"

	_ "image/gif"
	_ "image/jpeg"
	_ "image/png"

	"github.com/awused/aw-man/internal/closing"
	"github.com/awused/aw-man/internal/config"
	"github.com/awused/aw-man/internal/manager"
	"github.com/awused/aw-man/internal/resources"
	log "github.com/sirupsen/logrus"
	"golang.org/x/image/draw"

	"gioui.org/app"
	"gioui.org/font/opentype"
	"gioui.org/io/event"
	"gioui.org/io/key"
	"gioui.org/io/pointer"
	"gioui.org/io/system"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/clip"
	"gioui.org/op/paint"
	"gioui.org/text"
	"gioui.org/unit"
	"gioui.org/widget"
	"gioui.org/widget/material"
)

// Contains only the information to draw the GUI at this point in time
type gui struct {
	window *app.Window
	theme  *material.Theme

	state           manager.State
	imageChanged    bool
	hideUI          bool
	commandQueue    []manager.Command
	commandChan     chan<- manager.Command
	stateChan       <-chan manager.State
	lastScrollEvent pointer.Event
	imgOp           paint.ImageOp

	firstPaint      bool
	firstImagePaint bool

	imageSize     image.Point
	prevImageSize image.Point
	sizeChan      chan<- image.Point
}

var startTime time.Time = time.Now()
var commandTime time.Time = time.Now()

func (g *gui) sendCommand(c manager.Command) {
	// Queue the command for later if it can't be sent immediately
	commandTime = time.Now()
	select {
	case g.commandChan <- c:
	default:
		g.commandQueue = append(g.commandQueue, c)
	}
}

func curryCommand(c manager.Command) func(*gui) {
	return func(g *gui) {
		g.sendCommand(c)
	}
}

var internalCommands = map[string]func(*gui){
	"NextPage":        curryCommand(manager.NextPage),
	"PreviousPage":    curryCommand(manager.PrevPage),
	"LastPage":        curryCommand(manager.LastPage),
	"FirstPage":       curryCommand(manager.FirstPage),
	"NextArchive":     curryCommand(manager.NextArchive),
	"PreviousArchive": curryCommand(manager.PrevArchive),
	"ToggleUpscaling": curryCommand(manager.UpscaleToggle),
	"MangaMode":       curryCommand(manager.MangaToggle),
	"HideUI":          func(g *gui) { g.hideUI = !g.hideUI },
	"Quit":            func(g *gui) { g.window.Close() },
}

var commandTable = map[string]manager.Command{
	key.NamePageDown:  manager.NextPage,
	key.NamePageUp:    manager.PrevPage,
	key.NameEnd:       manager.LastPage,
	key.NameHome:      manager.FirstPage,
	key.NameDownArrow: manager.NextPage,
	key.NameUpArrow:   manager.FirstPage,
	"]":               manager.NextArchive,
	"[":               manager.PrevArchive,
	"U":               manager.UpscaleToggle,
}

// Modifiers bitmask -> uppercase key name -> action name
var shortcuts = map[key.Modifiers]map[string]string{}

func (g *gui) processEvents(evs []event.Event) {
	for _, e := range evs {
		switch e := e.(type) {
		case pointer.Event:
			if e.Type != pointer.Scroll {
				continue
			}
			oldScrollEvent := g.lastScrollEvent
			g.lastScrollEvent = e
			if oldScrollEvent == e {
				continue
			}

			if e.Scroll.Y > 0 {
				g.sendCommand(manager.NextPage)
			} else {
				g.sendCommand(manager.PrevPage)
			}

		case key.Event:
			if e.State != key.Press {
				continue
			}
			s := shortcuts[e.Modifiers][strings.ToUpper(e.Name)]
			log.Println(e, s)
			if s == "" {
				continue
			}
			if fn, ok := internalCommands[s]; ok {
				fn(g)
				continue
			}
			// It's a custom executable, go do it.
			//g.runExecutable(s)
		}
	}
}

func (g *gui) handleInput(gtx layout.Context, e system.FrameEvent) {
	g.processEvents(e.Queue.Events(g))

	pointer.InputOp{
		Tag:          g,
		ScrollBounds: image.Rect(0, -1, 0, 1),
	}.Add(gtx.Ops)
	key.InputOp{Tag: g}.Add(gtx.Ops)
	key.FocusOp{Tag: g}.Add(gtx.Ops)

}

func (g *gui) handleState(gs manager.State) {
	if g.state.Image != gs.Image {
		g.imageChanged = true
	}

	if g.state.ArchiveName != gs.ArchiveName {
		g.window.Option(app.Title(gs.ArchiveName + " - aw-man"))
	}

	g.state = gs

	g.window.Invalidate()
}

func (g *gui) drawImage() func(gtx layout.Context) layout.Dimensions {
	return func(gtx layout.Context) layout.Dimensions {
		sz := gtx.Constraints.Max
		if sz.X == 0 || sz.Y == 0 {
			return layout.Dimensions{}
		}

		if sz != g.imageSize {
			g.imageSize = sz
			// Scaling the image in the UI can take long enough that it'd be nicer to signal
			// the manager before scaling.
			select {
			case g.sizeChan <- sz:
				commandTime = time.Now()
				g.prevImageSize = sz
			default:
			}
		}

		img := g.state.Image
		if img == nil {
			return layout.Dimensions{}
		}
		if img.Bounds().Size().X == 0 || img.Bounds().Size().Y == 0 {
			log.Errorln("Tried to display 0 sized image", g.state)
			return layout.Dimensions{}
		}

		r := manager.CalculateImageBounds(g.state.OriginalBounds, sz)
		if g.imgOp.Size() != r.Bounds().Size() || g.imageChanged {
			if r == img.Bounds() {
				if g.imageChanged {
					g.imgOp = paint.NewImageOp(img)
				}
				// } else if !g.imageChanged &&
				// 	math.Abs(1-float64(r.Size().X)/float64(g.imgOp.Size().X)) < 0.1 &&
				// 	math.Abs(1-float64(r.Size().Y)/float64(g.imgOp.Size().Y)) < 0.1 {
				// 	// Skip scaling by tiny factors.
			} else {
				s := time.Now()
				log.Infoln(
					"Needed to scale at draw time", img.Bounds().Size(), "->", r.Size(), sz)
				rgba := image.NewRGBA(r)
				// TODO -- consider being more intelligent here.
				draw.ApproxBiLinear.Scale(rgba,
					r,
					img,
					img.Bounds(),
					draw.Src, nil)
				log.Debugln("Image scale time", time.Now().Sub(s))
				g.imgOp = paint.NewImageOp(rgba)
			}
			g.firstImagePaint = true
		}

		return widget.Image{
			Src:      g.imgOp,
			Scale:    float32(r.Size().X) / float32(gtx.Px(unit.Dp(float32(r.Size().X)))),
			Position: layout.Center,
		}.Layout(gtx)
	}
}

func (g *gui) drawBottomBar() func(gtx layout.Context) layout.Dimensions {
	return func(gtx layout.Context) layout.Dimensions {
		if g.hideUI {
			return layout.Dimensions{}
		}

		left := strconv.Itoa(g.state.PageNumber) + " / " + strconv.Itoa(g.state.ArchiveLength) +
			"   |   " + g.state.ArchiveName + "   |   " + g.state.PageName

		bar := layout.Stacked(func(gtx layout.Context) layout.Dimensions {
			return layout.Inset{
				Top:    unit.Dp(4),
				Bottom: unit.Dp(4),
				Left:   unit.Dp(30),
				Right:  unit.Dp(30),
			}.Layout(
				gtx, func(gtx layout.Context) layout.Dimensions {
					return layout.Flex{
						Axis: layout.Horizontal,
					}.Layout(gtx,
						layout.Rigid(material.Body2(
							g.theme,
							left,
						).Layout),
						layout.Flexed(1, func(gtx layout.Context) layout.Dimensions {
							return layout.Dimensions{
								Size: gtx.Constraints.Min,
							}
						}),
					)
				},
			)
		})

		bg := layout.Expanded(func(gtx layout.Context) layout.Dimensions {
			paint.FillShape(
				gtx.Ops,
				g.theme.Bg,
				clip.Rect{Max: gtx.Constraints.Min}.Op())

			return layout.Dimensions{Size: gtx.Constraints.Min}
		})

		return layout.Stack{}.Layout(gtx, bg, bar)
	}
}

func (g *gui) run(
	wg *sync.WaitGroup) {
	defer wg.Done()
	defer func() {
		for range g.window.Events() {
			// Just run down all the events so it can clean up and die.
			// If we get stuck here it shouldn't matter.
		}
	}()
	defer func() {
		if r := recover(); r != nil {
			log.Errorln("Gui panic: \n" + string(debug.Stack()))
			closing.Once()
		}
	}()

	parseShortcuts()
	wClosed := false

	g.theme = material.NewTheme(loadFonts())
	g.theme.Palette = material.Palette{
		Bg: color.NRGBA{R: 0x42, G: 0x42, B: 0x42, A: 0xff},
		Fg: color.NRGBA{R: 0xee, G: 0xee, B: 0xee, A: 0xff},
		//ContrastBg: color.NRGBA{R: 0x42, G: 0x42, B: 0x42, A: 0xff}
		//ContrastFg: color.NRGBA{R: 0xee, G: 0xee, B: 0xee, A: 0xff},
	}

	var ops op.Ops
	for {
		var sizeCh chan<- image.Point
		var cmdCh chan<- manager.Command
		var cmdToSend manager.Command
		if g.imageSize != g.prevImageSize {
			sizeCh = g.sizeChan
		}

		if len(g.commandQueue) > 0 {
			cmdToSend = g.commandQueue[0]
			cmdCh = g.commandChan
		}

		select {
		case <-closing.Ch:
			if !wClosed {
				g.window.Close()
			}
			return
		case gs := <-g.stateChan:
			g.handleState(gs)
		case sizeCh <- g.imageSize:
			commandTime = time.Now()
			g.prevImageSize = g.imageSize
		case cmdCh <- cmdToSend:
			g.commandQueue = g.commandQueue[1:]
		case e := <-g.window.Events():
			switch e := e.(type) {
			case system.FrameEvent:
				frameStart := time.Now()
				g.firstPaint = true
				firstImagePaint := g.firstImagePaint

				gtx := layout.NewContext(&ops, e)

				if *config.DebugFlag && g.state.Image != nil && !firstImagePaint {
					log.Debugln("Time until first image paint started", time.Now().Sub(startTime))
				}

				paint.ColorOp{
					Color: color.NRGBA{R: 0x13, G: 0x13, B: 0x13, A: 0xFF},
				}.Add(&ops)
				paint.PaintOp{}.Add(&ops)

				layout.Flex{
					Axis:    layout.Vertical,
					Spacing: layout.SpaceStart,
				}.Layout(gtx,
					layout.Flexed(1, g.drawImage()),
					layout.Rigid(g.drawBottomBar()),
				)

				g.handleInput(gtx, e)
				e.Frame(gtx.Ops)
				if !firstImagePaint && g.firstImagePaint {
					log.Infoln("Time until first image visible", time.Now().Sub(startTime))
				}
				rdTime := time.Now().Sub(frameStart)
				if rdTime > 100*time.Millisecond {
					log.Infoln("Redraw time", time.Now().Sub(frameStart))
				} else if rdTime > 16*time.Millisecond {
					log.Debugln("Redraw time", time.Now().Sub(frameStart))
				}

				if g.imageChanged {
					d := time.Now().Sub(commandTime)
					if d > 100*time.Millisecond {
						log.Infoln("Time from user action to image change", time.Now().Sub(commandTime))
					} else if d > 20*time.Millisecond {
						log.Debugln("Time from user action to image change", time.Now().Sub(commandTime))
					}
				}
				g.imageChanged = false
			case system.DestroyEvent:
				wClosed = true
				closing.Once()
				if e.Err != nil {
					log.Errorln(e.Err)
				}
			}
		}
	}
}

func loadFonts() []text.FontFace {
	regular, err := opentype.Parse(resources.NotoSansRegular)
	if err != nil {
		log.Panicln("Error parsing embedded NotoSansRegular", err)
	}
	return []text.FontFace{
		{Font: text.Font{}, Face: regular},
	}
}

func parseShortcuts() {
	for _, s := range config.Conf.Shortcuts {
		var mods key.Modifiers
		sm := strings.ToLower(s.Modifiers)
		if strings.Contains(sm, "control") {
			mods |= key.ModCtrl
		}
		if strings.Contains(sm, "alt") {
			mods |= key.ModAlt
		}
		if strings.Contains(sm, "shift") {
			mods |= key.ModShift
		}
		if strings.Contains(sm, "super") {
			mods |= key.ModSuper
		}
		if strings.Contains(sm, "command") {
			mods |= key.ModCommand
		}
		k := strings.ToUpper(s.Key)
		if _, ok := shortcuts[mods]; !ok {
			shortcuts[mods] = make(map[string]string)
		}
		shortcuts[mods][k] = s.Action
	}
}
