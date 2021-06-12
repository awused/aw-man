package closing

import "sync"

var Ch chan struct{} = make(chan struct{})
var closeSync sync.Once

func Once() {
	closeSync.Do(func() { close(Ch) })
}
