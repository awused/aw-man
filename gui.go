package main

import (
	"image"
	"image/color"
	"sync"
	"time"

	_ "image/gif"
	_ "image/jpeg"
	_ "image/png"

	"github.com/awused/aw-manga/internal/closing"
	"github.com/awused/aw-manga/internal/config"
	"github.com/awused/aw-manga/internal/manager"
	log "github.com/sirupsen/logrus"
	"golang.org/x/image/draw"

	"gioui.org/app"
	"gioui.org/font/gofont"
	"gioui.org/io/event"
	"gioui.org/io/key"
	"gioui.org/io/pointer"
	"gioui.org/io/system"
	"gioui.org/layout"
	"gioui.org/op"
	"gioui.org/op/paint"
	"gioui.org/unit"
	"gioui.org/widget"
	"gioui.org/widget/material"
)

// Contains only the information to draw the GUI at this point in time
type gui struct {
	state           manager.State
	stateChanged    bool
	firstPaint      bool
	window          *app.Window
	commandChan     chan<- manager.Command
	sizeChan        chan<- image.Point
	stateChan       <-chan manager.State
	lastScrollEvent pointer.Event
}

var startTime time.Time = time.Now()

func (g *gui) sendCommand(c manager.Command) {
	for {
		select {
		case g.commandChan <- c:
			return
		case gs := <-g.stateChan:
			g.handleState(gs)
		case <-closing.Ch:
			return
		}
	}
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
			c, ok := commandTable[e.Name]
			if ok {
				g.sendCommand(c)
				continue
			}
			switch e.Name {
			case key.NameEscape:
				g.window.Close()
			case "Q":
				g.window.Close()
			}
		}
	}
}

func (g *gui) handleState(gs manager.State) {
	g.state = gs
	g.stateChanged = true

	if g.firstPaint {
		g.window.Invalidate()
	}
}

func (g *gui) run(
	wg *sync.WaitGroup) {
	defer wg.Done()
	defer func() {
		for range g.window.Events() {
			// Just run down all the events so it can clean up and die
		}
	}()

	previousImageSize := image.Point{}
	newImageSize := image.Point{}

	firstImagePaint := false
	wClosed := false

	th := material.NewTheme(gofont.Collection())
	th.Palette.Fg = color.NRGBA{R: 0xb0, G: 0xb0, B: 0xb0, A: 0xFF}
	imgOp := paint.ImageOp{}

	var ops op.Ops
	for {
		var sizeCh chan<- image.Point
		if newImageSize != previousImageSize {
			sizeCh = g.sizeChan
		}

		select {
		case <-closing.Ch:
			if !wClosed {
				g.window.Close()
			}
			return
		case gs := <-g.stateChan:
			g.handleState(gs)
		case sizeCh <- newImageSize:
			previousImageSize = newImageSize
		case e := <-g.window.Events():
			switch e := e.(type) {
			case system.FrameEvent:
				g.firstPaint = true
				if *config.DebugFlag && g.state.Image != nil && !firstImagePaint {
					log.Debugln("Time until first image paint started", time.Now().Sub(startTime))
				}
				frameStart := time.Now()

				gtx := layout.NewContext(&ops, e)
				g.processEvents(e.Queue.Events(&g))

				pointer.InputOp{
					Tag:          &g,
					ScrollBounds: image.Rect(0, -1, 0, 1),
				}.Add(gtx.Ops)
				key.InputOp{Tag: &g}.Add(gtx.Ops)
				key.FocusOp{Tag: &g}.Add(gtx.Ops)

				paint.ColorOp{
					Color: color.NRGBA{R: 0x13, G: 0x13, B: 0x13, A: 0xFF},
				}.Add(&ops)
				paint.PaintOp{}.Add(&ops)

				layout.Flex{
					Axis: layout.Vertical,
				}.Layout(gtx,
					layout.Flexed(1, func(gtx layout.Context) layout.Dimensions {
						img := g.state.Image
						sz := gtx.Constraints.Max
						if sz.X == 0 || sz.Y == 0 {
							return layout.Spacer{}.Layout(gtx)
						}

						if sz != previousImageSize {
							newImageSize = sz
						}

						if img == nil {
							return layout.Spacer{}.Layout(gtx)
						}
						if img.Bounds().Size().X == 0 || img.Bounds().Size().Y == 0 {
							log.Errorln("Tried to display 0 sized image", g.state)
							return layout.Spacer{}.Layout(gtx)
						}

						r := manager.CalculateImageBounds(img.Bounds(), sz)
						if imgOp.Size() != r.Bounds().Size() || g.stateChanged {
							if r == img.Bounds() {
								if g.stateChanged {
									imgOp = paint.NewImageOp(img)
								}
							} else {
								s := time.Now()
								log.Debugln(
									"Needed to scale at draw time", img.Bounds().Size(), "->", r.Size())
								rgba := image.NewRGBA(r)
								manager.ScalingMethod.Scale(rgba,
									r,
									img,
									img.Bounds(),
									draw.Src, nil)
								if *config.DebugFlag {
									log.Debugln("Image scale time", time.Now().Sub(s))
								}
								imgOp = paint.NewImageOp(rgba)
							}
						}

						return widget.Image{
							Src:      imgOp,
							Scale:    float32(r.Size().X) / float32(gtx.Px(unit.Dp(float32(r.Size().X)))),
							Position: layout.Center,
						}.Layout(gtx)
					}),
					layout.Rigid(func(gtx layout.Context) layout.Dimensions {
						return layout.Dimensions{
							Size: image.Point{X: gtx.Constraints.Max.X, Y: 44},
						}
						//return material.Body1(th, "asdf").Layout(gtx)
					}),
				)

				g.stateChanged = false
				e.Frame(gtx.Ops)
				if *config.DebugFlag {
					rdTime := time.Now().Sub(frameStart)
					if rdTime > 16*time.Millisecond {
						log.Debugln("Redraw time", time.Now().Sub(frameStart))
					}
					if g.state.Image != nil && !firstImagePaint {
						firstImagePaint = true
						log.Debugln("Time until first image visible", time.Now().Sub(startTime))
					}
				}
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
