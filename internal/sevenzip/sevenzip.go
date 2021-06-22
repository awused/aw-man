package sevenzip

import (
	"bufio"
	"bytes"
	"io"
	"os"
	"os/exec"
	"strconv"
	"strings"

	log "github.com/sirupsen/logrus"
)

// SevenZipFile represents a file inside a 7zip archive
type SevenZipFile struct {
	Path string
	Size int64
}

// ListFiles will dump the list of files from the archive.
func ListFiles(path string) ([]SevenZipFile, error) {
	out, err := exec.Command("7z", "l", "-slt", "-sccUTF-8", path).Output()
	if err != nil {
		return nil, err
	}

	files := []SevenZipFile{}
	newF := SevenZipFile{}

	scanner := bufio.NewScanner(bytes.NewReader(out))
	for scanner.Scan() {
		line := scanner.Text()
		if strings.HasPrefix(line, "Path = ") {
			if newF.Path != "" && newF.Size != 0 {
				files = append(files, newF)
				newF = SevenZipFile{}
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
	return files, nil
}

// ExtractFile extracts a single file to the provided path
func ExtractFile(path string, filePath string, dst string) error {
	cmd := exec.Command("7z", "x", "-so", path, filePath)
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
	cmd := exec.Command("7z", "x", "-so", path)
	stdout, err := cmd.StdoutPipe()
	if err != nil {
		return nil, err
	}

	err = cmd.Start()

	return stdout, err
}
