package gui

import (
	"regexp"
	"strings"
	"time"

	"github.com/awused/aw-man/internal/config"
	"github.com/awused/aw-man/internal/manager"
	"github.com/gotk3/gotk3/gdk"
	"github.com/gotk3/gotk3/gtk"
	log "github.com/sirupsen/logrus"
)

func (g *gui) sendCommand(c manager.UserCommand) {
	// Queue the command for later if it can't be sent immediately
	commandTime = time.Now()
	select {
	case g.commandChan <- c:
	default:
		g.l.Lock()
		g.commandQueue = append(g.commandQueue, c)
		g.l.Unlock()
		select {
		case g.invalidChan <- struct{}{}:
		default:
		}
	}
}

func (g *gui) showBackgroundPicker() {
	dialog, err := gtk.ColorChooserDialogNew("Pick Background Colour", g.window)
	if err != nil {
		log.Panicln("Error opening colour picker dialog", err)
	}

	cc := dialog.ColorChooser

	cc.SetRGBA(g.bg)

	resp := dialog.Run()
	if resp == gtk.RESPONSE_OK {
		g.themeBG = false
		g.bg = cc.GetRGBA()
		g.widgets.canvas.QueueDraw()
	}

	dialog.Destroy()
}

func curryCommand(c manager.Command) func(*gui, string) {
	return func(g *gui, a string) {
		g.sendCommand(manager.UserCommand{Cmd: c, Arg: a})
	}
}

var simpleCommands = map[string]func(*gui, string){
	"NextPage":        curryCommand(manager.NextPage),
	"PreviousPage":    curryCommand(manager.PrevPage),
	"LastPage":        curryCommand(manager.LastPage),
	"FirstPage":       curryCommand(manager.FirstPage),
	"NextArchive":     curryCommand(manager.NextArchive),
	"PreviousArchive": curryCommand(manager.PrevArchive),
	"ToggleUpscaling": curryCommand(manager.UpscaleToggle),
	"MangaMode":       curryCommand(manager.MangaToggle),
	"Quit":            func(g *gui, _ string) { g.window.Close() },
	"ToggleUI": func(g *gui, _ string) {
		g.hideUI = !g.hideUI
		if g.hideUI {
			g.widgets.bottomBar.Hide()
		} else {
			g.widgets.bottomBar.Show()
		}
	},
	"ToggleThemeBackground": func(g *gui, _ string) {
		g.themeBG = !g.themeBG
		g.widgets.canvas.QueueDraw()
	},
	"SetBackground": func(g *gui, _ string) {
		g.showBackgroundPicker()
	},
	"ToggleFullscreen": func(g *gui, _ string) {
		if g.isFullscreen {
			g.window.Unfullscreen()
		} else {
			g.window.Fullscreen()
		}
	},
}

var argCommands = map[*regexp.Regexp]func(*gui, string){
	regexp.MustCompile("^SetBackground ([0-9a-fA-F]{8})$"): func(g *gui, a string) {
		g.setBackgroundRGBA(a)
		g.widgets.canvas.QueueDraw()
	},
}

// Modifiers bitmask -> uppercase key name -> action name
var shortcuts = map[gdk.ModifierType]map[uint]string{}

func (g *gui) handleKeyPress(win *gtk.Window, event *gdk.Event) {
	e := gdk.EventKeyNewFromEvent(event)
	mods := gdk.ModifierType(e.State())
	mods &= gdk.MODIFIER_MASK

	// We don't care about these
	mods = mods &^ (gdk.MOD2_MASK | gdk.LOCK_MASK)

	lower := gdk.KeyvalToUpper(e.KeyVal())
	s := shortcuts[mods][lower]
	log.Debugln(lower, mods, s)
	g.runCommand(s)
}

func (g *gui) runCommand(s string) {
	if s == "" {
		return
	}
	if fn, ok := simpleCommands[s]; ok {
		fn(g, "")
		return
	}
	for rg, fn := range argCommands {
		m := rg.FindStringSubmatch(s)
		if m != nil {
			fn(g, m[1])
			return
		}
	}
	// It's a custom executable, go do it.
	g.l.Lock()
	g.executableQueue = append(g.executableQueue, s)
	g.l.Unlock()
	select {
	case g.invalidChan <- struct{}{}:
	default:
	}
}

func (g *gui) handleScroll(da *gtk.DrawingArea, event *gdk.Event) {
	e := gdk.EventScrollNewFromEvent(event)
	switch e.Direction() {
	case gdk.SCROLL_DOWN:
		g.sendCommand(manager.UserCommand{Cmd: manager.NextPage})
	case gdk.SCROLL_UP:
		g.sendCommand(manager.UserCommand{Cmd: manager.PrevPage})
	}
}

func parseShortcuts() {
	for _, s := range config.Conf.Shortcuts {
		var mods gdk.ModifierType
		sm := strings.ToLower(s.Modifiers)
		if strings.Contains(sm, "control") {
			mods |= gdk.CONTROL_MASK
		}
		if strings.Contains(sm, "alt") {
			mods |= gdk.MOD1_MASK
		}
		if strings.Contains(sm, "shift") {
			mods |= gdk.SHIFT_MASK
		}
		if strings.Contains(sm, "super") {
			mods |= gdk.SUPER_MASK
		}
		if strings.Contains(sm, "command") {
			mods |= gdk.META_MASK
		}
		k := gdk.KeyvalFromName(s.Key)
		if _, ok := shortcuts[mods]; !ok {
			shortcuts[mods] = make(map[uint]string)
		}
		shortcuts[mods][k] = s.Action
	}
}
