package manager

import (
	"errors"
	"regexp"
	"strconv"

	"github.com/awused/aw-man/internal/config"
)

func (m *manager) moveNPages(n int) {
	if n == 0 {
		return
	}

	nc, ok := m.add(m.c, n)
	if ok {
		if !m.mangaMode && nc.a != m.c.a {
			if nc.a > m.c.a {
				nc.p = m.archives[m.c.a].PageCount() - 1
			} else {
				nc.p = 0
			}
			if nc.p < 0 {
				nc.p = 0
			}
			nc.a = m.c.a
		}
	} else if m.mangaMode {
		// This will recursively open more archives, which can be fairly slow and inefficient since we
		// begin extracting immediately in parallel. Unless the user is doing something really dumb,
		// this should cap out at a few chapters and isn't worth optimizing.
		// If the user is doing something dumb, let them shoot themselves in the foot.
		// Limited by Go's max stack size and the room available for extracting images.
		if n > 0 {
			mode := preloading
			if nc.p == m.archives[nc.a].PageCount() {
				mode = waitingOnFirst
			}
			if m.openNextArchive(mode) != nil {
				m.moveNPages(n)
				return
			}
		} else {
			mode := preloading
			if nc.p == -1 {
				mode = waitingOnLast
			}
			if m.openPreviousArchive(mode) != nil {
				m.moveNPages(n)
				return
			}
		}
	} else {
		nc.a = m.c.a
	}
	if nc.p >= m.archives[nc.a].PageCount() {
		nc.p = m.archives[nc.a].PageCount() - 1
	}
	if nc.p < 0 {
		nc.p = 0
	}
	oldc := m.c
	m.c = nc
	m.afterMove(oldc)
}

func (m *manager) nextPage() {
	m.moveNPages(1)
}

func (m *manager) prevPage() {
	m.moveNPages(-1)
}

var jumpRe = regexp.MustCompile(`^(\+|-)?(\d+)$`)

func (m *manager) jump(arg string) error {
	match := jumpRe.FindStringSubmatch(arg)
	if match == nil {
		return errors.New("Jump command had invalid argument" + arg)
	}

	j, err := strconv.Atoi(match[2])
	if err != nil {
		return err
	}

	a := m.archives[m.c.a]
	oldc := m.c

	// Absolute jump never crosses chapter boundaries
	if match[1] == "" {
		// Users would expect one-indexed pages.
		j--
		if j >= len(a.pages) {
			j = len(a.pages) - 1
		}
		if j < 0 {
			j = 0
		}
		m.c.p = j
		m.afterMove(oldc)
		return nil
	}

	if match[1] == "-" {
		j = j * -1
	}
	m.moveNPages(j)
	return nil
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

func (m *manager) mangaToggle() {
	m.mangaMode = !m.mangaMode
	if m.mangaMode {
		m.afterMove(m.c)
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

	m.closeUnusedArchives(newStart, newEnd, firstUpscaled, lastUpscaled)

	// When cleaning up archives, be sure to adjust indices
	m.firstImageFromFile = nil
}
