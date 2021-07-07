package manager

import (
	log "github.com/sirupsen/logrus"
)

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
// The archive will be a valid index, but the page may not be.
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
