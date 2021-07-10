package sevenzip

import (
	"bufio"
	"bytes"
	"errors"
	"io"
	"os"
	"os/exec"
	"strconv"
	"strings"

	"github.com/awused/aw-man/internal/config"
	log "github.com/sirupsen/logrus"
)

var has7z = false

var (
	errDisabled = errors.New("External extractors disabled")
	errNotFound = errors.New("7zip executable not found")
)

// File represents a file inside a 7zip archive
type File struct {
	Path string
	Size int64
}

func init() {
	_, e := exec.LookPath("7z")
	has7z = e == nil
}

// Enabled returns true if the executable was found and is allowed by the user.
func Enabled() bool {
	return has7z && config.Conf.AllowExternalExtractors
}

// GetMetadata will dump the list of files from the archive and return its kind.
func GetMetadata(path string) ([]File, string, error) {
	if !config.Conf.AllowExternalExtractors {
		return nil, "", errDisabled
	}

	if !has7z {
		return nil, "", errNotFound
	}

	out, err := exec.Command("7z", "l", "-slt", "-sccUTF-8", "--", path).Output()
	if err != nil {
		return nil, "", err
	}

	files := []File{}
	newF := File{}
	kind := ""

	scanner := bufio.NewScanner(bytes.NewReader(out))
	for scanner.Scan() {
		line := scanner.Text()
		if strings.HasPrefix(line, "Type = ") && kind == "" {
			kind = strings.TrimPrefix(line, "Type = ")
			kind = strings.ToLower(kind)
		}

		if strings.HasPrefix(line, "Path = ") {
			if newF.Path != "" && newF.Size != 0 {
				files = append(files, newF)
				newF = File{}
			}

			f := strings.TrimPrefix(line, "Path = ")
			if f != path {
				newF.Path = f
			}
		}
		if strings.HasPrefix(line, "Size = ") {
			s := strings.TrimPrefix(line, "Size = ")
			size, err := strconv.ParseInt(s, 10, 64)
			if err != nil {
				log.Errorln("Invalid size inside 7z archive", s)
			} else {
				newF.Size = size
			}
		}
	}

	if newF.Path != "" && newF.Size != 0 {
		files = append(files, newF)
	}
	return files, kind, nil
}

// ExtractFile extracts a single file to the provided path
func ExtractFile(path string, filePath string, dst string) error {
	if !config.Conf.AllowExternalExtractors {
		return errDisabled
	}

	if !has7z {
		return errNotFound
	}

	cmd := exec.Command("7z", "x", "-so", "--", path, filePath)
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return err
	}
	defer stdout.Close()

	err = cmd.Start()
	if err != nil {
		return err
	}

	outF, err := os.Create(dst)
	if err != nil {
		return err
	}
	defer outF.Close()

	_, err = io.Copy(outF, stdout)
	return err
}

// GetReader returns an io.ReadCloser for the entire archive.
func GetReader(path string) (io.ReadCloser, error) {
	if !config.Conf.AllowExternalExtractors {
		return nil, errDisabled
	}

	if !has7z {
		return nil, errNotFound
	}

	cmd := exec.Command("7z", "x", "-so", "--", path)
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, err
	}

	err = cmd.Start()

	return stdout, err
}
