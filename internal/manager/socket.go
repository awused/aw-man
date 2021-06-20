package manager

import (
	"encoding/json"
	"net"
	"strings"
	"time"

	log "github.com/sirupsen/logrus"
)

func (m *manager) handleConn(c net.Conn) {
	defer c.Close()
	// We're blocking on this to keep the code simple, so set a short deadline.
	c.SetDeadline(time.Now().Add(50 * time.Millisecond))
	b := make([]byte, 128)
	n, err := c.Read(b)
	if err != nil {
		log.Errorln("Socket error", err)
	}

	req := strings.TrimSpace(string(b[:n]))
	switch req {
	case "status":
		err = json.NewEncoder(c).Encode(m.getStateEnvVars())
		if err != nil {
			log.Errorln("Socket error", err)
		}
	default:
		c.Write([]byte("\"Unknown request.\""))
	}
}
