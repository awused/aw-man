package manager

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
