package manager

import (
	"encoding/json"
	"net"
	"os"
	"os/exec"
	"strconv"
	"strings"
	"time"

	"github.com/awused/aw-man/internal/config"
	log "github.com/sirupsen/logrus"
)

func (m *manager) getStateEnvVars() map[string]string {
	env := make(map[string]string)

	ca, cp, _ := m.get(m.c)

	env["AWMAN_ARCHIVE"] = ca.path
	env["AWMAN_ARCHIVE_TYPE"] = ca.kind.String()
	env["AWMAN_PID"] = strconv.Itoa(os.Getpid())

	if cp != nil {
		env["AWMAN_RELATIVE_FILE_PATH"] = cp.inArchivePath
		env["AWMAN_PAGE_NUMBER"] = strconv.Itoa(m.c.p + 1)

		if cp.state >= extracted {
			env["AWMAN_CURRENT_FILE"] = cp.file
		}
	}

	return env
}

func (m *manager) runExecutable(e Executable) {
	env := m.getStateEnvVars()
	// Fire and forget. If it's still running when the program exits we just don't care.
	go func() {
		if e.Ch != nil {
			defer close(e.Ch)
		}
		cmd := exec.Command(e.Exec)
		cmd.Env = os.Environ()
		for k, v := range env {
			cmd.Env = append(cmd.Env, k+"="+v)
		}

		// Don't spawn a console on Windows
		cmd.SysProcAttr = config.SysProcAttr

		out, err := cmd.CombinedOutput()
		if err != nil {
			log.Errorln("Executable", e, "from exited with error", err)
			log.Infoln("Output:", string(out))
			if e.Ch != nil {
				e.Ch <- err
			}
		} else if len(out) != 0 {
			log.Infoln("Ran", e, "with output:", string(out))
		}
	}()
}

func (m *manager) handleConn(c net.Conn) {
	// We're blocking on this to keep the code simple, so set a short read deadline.
	// We don't care at all if socket connections are open when the program exits.
	err := c.SetReadDeadline(time.Now().Add(50 * time.Millisecond))
	if err != nil {
		log.Errorln("Socket error", err)
	}
	b := make([]byte, 128)
	n, err := c.Read(b)
	if err != nil {
		log.Errorln("Socket error", err)
		c.Close()
		return
	}

	req := strings.TrimSpace(string(b[:n]))
	switch req {
	case "status":
		e := m.getStateEnvVars()
		go func() {
			defer c.Close()
			err = json.NewEncoder(c).Encode(e)
			if err != nil {
				log.Errorln("Socket error", err)
			}
		}()
	default:
		ch := make(chan error)
		sc := SocketCommand{
			Cmd: req,
			Ch:  ch,
		}
		m.socketCommands = append(m.socketCommands, sc)
		go func() {
			defer c.Close()
			err := <-ch
			if err != nil {
				c.Write([]byte("\"" + err.Error() + "\""))
			} else {
				c.Write([]byte("\"done\""))
			}
		}()
	}
}
