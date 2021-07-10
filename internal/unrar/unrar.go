package unrar

import (
	"bufio"
	"bytes"
	"errors"
	"io"
	"os"
	"os/exec"
	"regexp"
	"strconv"
	"strings"

	"github.com/awused/aw-man/internal/config"
	log "github.com/sirupsen/logrus"
)

var hasunrar = false

var (
	errDisabled = errors.New("External extractors disabled")
	errNotFound = errors.New("unrar executable not found")
)

// File represents a file inside a 7zip archive
type File struct {
	Path string
	Size int64
}

func init() {
	_, e := exec.LookPath("unrar")
	hasunrar = e == nil
}

// Enabled returns true if the executable was found and is allowed by the user.
func Enabled() bool {
	return hasunrar && config.Conf.AllowExternalExtractors
}

var fileLine = regexp.MustCompile(`^.* (\d+) +[^ ]+ +[^ ]+ +(.*)$`)

// GetMetadata will dump the list of files from the archive.
func GetMetadata(path string) ([]File, error) {
	if !config.Conf.AllowExternalExtractors {
		return nil, errDisabled
	}

	if !hasunrar {
		return nil, errNotFound
	}

	out, err := exec.Command("unrar", "l", "--", path).Output()
	if err != nil {
		return nil, err
	}

	files := []File{}
	newF := File{}
	kind := ""

	scanner := bufio.NewScanner(bytes.NewReader(out))
	for scanner.Scan() {
		line := scanner.Text()
		match := fileLine.FindStringSubmatch(line)
		if match == nil {
			continue
		}
		if strings.HasPrefix(line, "Type = ") && kind == "" {
			kind = strings.TrimPrefix(line, "Type = ")
			kind = strings.ToLower(kind)
		}

		size, err := strconv.ParseInt(match[1], 10, 64)
		if err != nil {
			log.Errorln("Invalid size inside rar archive", match[1])
			continue
		} else {
			newF.Size = size
		}
		files = append(files, File{
			Path: match[2],
			Size: size,
		})
	}

	return files, nil
}

// ExtractFile extracts a single file to the provided path
func ExtractFile(path string, filePath string, dst string) error {
	if !config.Conf.AllowExternalExtractors {
		return errDisabled
	}

	if !hasunrar {
		return errNotFound
	}

	cmd := exec.Command("unrar", "p", "-inul", "--", path, filePath)
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

	if !hasunrar {
		return nil, errNotFound
	}

	cmd := exec.Command("unrar", "p", "-inul", "--", path)
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, err
	}

	err = cmd.Start()

	return stdout, err
}
