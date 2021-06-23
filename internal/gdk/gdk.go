// +build gdk

package gdk

/*
#cgo pkg-config: gdk-3.0 glib-2.0 gobject-2.0
#include <gdk/gdk.h>
#include <glib.h>
#include <malloc.h>

// Copied from https://github.com/gotk3/gotk3, see LICENSE below
static inline gchar **next_gcharptr(gchar **s) { return (s + 1); }

int _gdk_pixbuf_save_png(GdkPixbuf *pixbuf, const char *filename, GError **err,
                     const char *compression) {
  return gdk_pixbuf_save(pixbuf, filename, "png", err, "compression",
                         compression, NULL);
}

static GdkPixbuf *toGdkPixbuf(void *p) { return (GDK_PIXBUF(p)); }
*/
import "C"
import (
	"errors"
	"path/filepath"
	"runtime"
	"strconv"
	"strings"
	"sync"
	"unsafe"
)

var ignoredExtensions = map[string]bool{}

var supportedExtensions = map[string]bool{}

var once sync.Once

// IsSupportedImage returns if the image is supported by gdk,
// unless it is a format that is deliberately ignored.
func IsSupportedImage(f string) bool {
	once.Do(func() {
		C.gdk_init(nil, nil)
		PixbufGetFormats()
	})

	ext := strings.ToLower(filepath.Ext(f))
	return supportedExtensions[ext]
}

// ConvertImageToPNG converts the image at src to a png and writes it to dst
func ConvertImageToPNG(src, dst string) error {
	runtime.LockOSThread()
	defer runtime.UnlockOSThread()
	pb, err := PixbufNewFromFile(src)
	if err != nil {
		return err
	}
	defer C.malloc_trim(0)
	defer runtime.KeepAlive(pb)
	// Must unref specifically from the same thread.
	defer pb.Unref()

	return pb.SavePNG(dst, 5)
}

/*
ISC License

Copyright (c) 2013-2014 Conformal Systems LLC.
Copyright (c) 2015-2018 gotk3 contributors

Permission to use, copy, modify, and/or distribute this software for any
purpose with or without fee is hereby granted, provided that the above
copyright notice and this permission notice appear in all copies.

THE SOFTWARE IS PROVIDED "AS IS" AND THE AUTHOR DISCLAIMS ALL WARRANTIES
WITH REGARD TO THIS SOFTWARE INCLUDING ALL IMPLIED WARRANTIES OF
MERCHANTABILITY AND FITNESS. IN NO EVENT SHALL THE AUTHOR BE LIABLE FOR
ANY SPECIAL, DIRECT, INDIRECT, OR CONSEQUENTIAL DAMAGES OR ANY DAMAGES
WHATSOEVER RESULTING FROM LOSS OF USE, DATA OR PROFITS, WHETHER IN AN
ACTION OF CONTRACT, NEGLIGENCE OR OTHER TORTIOUS ACTION, ARISING OUT OF
OR IN CONNECTION WITH THE USE OR PERFORMANCE OF THIS SOFTWARE.
*/

func gobool(b C.gboolean) bool {
	if b != 0 {
		return true
	}
	return false
}

type Object struct {
	GObject *C.GObject
}

func (v *Object) Ref() {
	C.g_object_ref(C.gpointer(v.GObject))
}

// Unref is a wrapper around g_object_unref().
func (v *Object) Unref() {
	C.g_object_unref(C.gpointer(v.GObject))
}

func ToGObject(p unsafe.Pointer) *C.GObject {
	return (*C.GObject)(p)
	// return C.toGObject(p)
}

func WrapSList(obj uintptr) *SList {
	return wrapSList((*C.struct__GSList)(unsafe.Pointer(obj)))
}

type SList struct {
	list *C.struct__GSList
	// If set, dataWrap is called every time Data()
	// is called to wrap raw underlying
	// value into appropriate type.
	dataWrap func(unsafe.Pointer) interface{}
}

func wrapSList(obj *C.struct__GSList) *SList {
	if obj == nil {
		return nil
	}

	//NOTE a list should be freed by calling either
	//g_slist_free() or g_slist_free_full(). However, it's not possible to use a
	//finalizer for this.
	return &SList{list: obj}
}

func (v *SList) wrapNewHead(obj *C.struct__GSList) *SList {
	if obj == nil {
		return nil
	}
	return &SList{
		list:     obj,
		dataWrap: v.dataWrap,
	}
}

// Next is a wrapper around the next struct field
func (v *SList) Next() *SList {
	n := v.native()
	if n == nil {
		return nil
	}

	return wrapSList(n.next)
}
func (v *SList) native() *C.struct__GSList {
	if v == nil || v.list == nil {
		return nil
	}
	return v.list
}

// dataRaw is a wrapper around the data struct field
func (v *SList) dataRaw() unsafe.Pointer {
	n := v.native()
	if n == nil {
		return nil
	}
	return unsafe.Pointer(n.data)
}

// Data acts the same as data struct field, but it returns raw unsafe.Pointer as interface.
// TODO: Align with List struct and add member + logic for `dataWrap func(unsafe.Pointer) interface{}`?
func (v *SList) Data() interface{} {
	ptr := v.dataRaw()
	if v.dataWrap != nil {
		return v.dataWrap(ptr)
	}
	return ptr
}

// Foreach acts the same as g_slist_foreach().
// No user_data argument is implemented because of Go clojure capabilities.
func (v *SList) Foreach(fn func(item interface{})) {
	for l := v; l != nil; l = l.Next() {
		fn(l.Data())
	}
}

// Free is a wrapper around g_slist_free().
func (v *SList) Free() {
	C.g_slist_free(v.native())
}

type Pixbuf struct {
	*Object
}

// native returns a pointer to the underlying GdkPixbuf.
func (v *Pixbuf) native() *C.GdkPixbuf {
	if v == nil || v.GObject == nil {
		return nil
	}
	p := unsafe.Pointer(v.GObject)
	return C.toGdkPixbuf(p)
}

// PixbufNewFromFile is a wrapper around gdk_pixbuf_new_from_file().
func PixbufNewFromFile(filename string) (*Pixbuf, error) {
	cstr := C.CString(filename)
	defer C.free(unsafe.Pointer(cstr))

	var err *C.GError
	c := C.gdk_pixbuf_new_from_file((*C.char)(cstr), &err)
	if c == nil {
		defer C.g_error_free(err)
		return nil, errors.New(C.GoString((*C.char)(err.message)))
	}

	obj := &Object{ToGObject(unsafe.Pointer(c))}
	p := &Pixbuf{obj}
	// THIS ISN'T THREADSAFE
	//runtime.SetFinalizer(p, func(_ interface{}) { obj.Unref() })
	return p, nil
}

// PixbufGetFormats is a wrapper around gdk_pixbuf_get_formats().
func PixbufGetFormats() {
	l := (*C.struct__GSList)(C.gdk_pixbuf_get_formats())
	formats := WrapSList(uintptr(unsafe.Pointer(l)))
	if formats == nil {
		return // no error. A nil list is considered to be empty.
	}

	// "The structures themselves are owned by GdkPixbuf". Free the list only.
	defer formats.Free()

	formats.Foreach(func(item interface{}) {
		//ret = append(ret, &gdk.PixbufFormat{item.(*C.GdkPixbufFormat)})
		c := C.gdk_pixbuf_format_get_extensions((*C.GdkPixbufFormat)(item.(unsafe.Pointer)))
		if c == nil {
			return
		}
		for *c != nil {
			e := "." + C.GoString((*C.char)(*c))
			if !ignoredExtensions[e] {
				supportedExtensions[e] = true
			}
			c = C.next_gcharptr(c)
		}
	})
}

// SavePNG is a convenience wrapper around gdk_pixbuf_save() for saving image as png file.
// Compression is a number between 0...9
func (v *Pixbuf) SavePNG(path string, compression int) error {
	cpath := C.CString(path)
	ccompression := C.CString(strconv.Itoa(compression))
	defer C.free(unsafe.Pointer(cpath))
	defer C.free(unsafe.Pointer(ccompression))

	var err *C.GError
	c := C._gdk_pixbuf_save_png(v.native(), cpath, &err, ccompression)
	if !gobool(c) {
		defer C.g_error_free(err)
		return errors.New(C.GoString((*C.char)(err.message)))
	}
	return nil
}
