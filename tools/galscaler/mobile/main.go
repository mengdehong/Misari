package main

import (
	"bytes"
	"context"
	_ "embed"
	"encoding/json"
	"errors"
	"io"
	"log"
	"net/http"
	"net/url"
	"slices"
	"strconv"
	"strings"
	"sync"
	"time"

	"github.com/egoist/mygo"
)

//go:embed player.html
var player string

type Mobile struct {
	mu     sync.RWMutex
	server url.URL
	client *http.Client
}

type Server struct {
	Name    string `json:"name"`
	Address string `json:"address"`
}

func (m *Mobile) Connect(address string) (string, error) {
	address = strings.TrimSpace(address)
	if !strings.Contains(address, "://") {
		address = "http://" + address
	}
	server, err := url.Parse(address)
	if err != nil || (server.Scheme != "http" && server.Scheme != "https") ||
		server.Hostname() == "" || server.User != nil || server.RawQuery != "" ||
		server.Fragment != "" || (server.Path != "" && server.Path != "/") {
		return "", errors.New("请输入有效的 GalScaler 地址，如 192.168.1.20:9771")
	}
	server.Path = ""
	m.mu.Lock()
	m.server = *server
	m.mu.Unlock()
	return server.String(), nil
}

func (*Mobile) Discover() ([]Server, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 3*time.Second)
	defer cancel()
	var found map[string]string
	if err := mygo.Native("discovery").Call(ctx, "list", nil, &found); err != nil {
		return nil, err
	}
	servers := make([]Server, 0, len(found))
	for name, address := range found {
		servers = append(servers, Server{Name: name, Address: address})
	}
	slices.SortFunc(servers, func(a, b Server) int { return strings.Compare(a.Name, b.Name) })
	return servers, nil
}

func (*Mobile) Playing(active bool) {
	if active {
		mygo.SetOrientations(mygo.OrientationLandscape)
	} else {
		mygo.SetOrientations(mygo.OrientationPortrait)
	}
	mygo.SystemBars.SetHidden(active)
}

type Response struct {
	Status int             `json:"status"`
	Reply  json.RawMessage `json:"reply"`
}

func (m *Mobile) Request(ctx context.Context, path, token string, body json.RawMessage) (Response, error) {
	if path != "/api/v1/rpc" {
		id, valid := strings.CutPrefix(path, "/api/v1/stream/")
		if _, err := strconv.ParseUint(id, 10, 64); !valid || err != nil {
			return Response{}, errors.New("无效的 GalScaler 请求")
		}
	}
	m.mu.RLock()
	server := m.server
	m.mu.RUnlock()
	server.Path = path
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, server.String(), bytes.NewReader(body))
	if err != nil {
		return Response{}, errors.New("电脑地址无效")
	}
	req.Header.Set("Content-Type", "application/json")
	req.Header.Set("Authorization", "Bearer "+token)
	resp, err := m.client.Do(req)
	if err != nil {
		return Response{}, errors.New("无法连接 GalScaler，请检查电脑地址和局域网")
	}
	defer resp.Body.Close()
	result := Response{Status: resp.StatusCode}
	if err := json.NewDecoder(io.LimitReader(resp.Body, 1<<20)).Decode(&result.Reply); err != nil {
		return Response{}, errors.New("电脑地址未返回 GalScaler 响应")
	}
	return result, nil
}

func servePlayer(w http.ResponseWriter, r *http.Request) {
	if r.Method != http.MethodGet || r.URL.Path != "/" {
		http.NotFound(w, r)
		return
	}
	w.Header().Set("Cache-Control", "no-store")
	w.Header().Set("Content-Type", "text/html; charset=utf-8")
	w.Header().Set("Content-Security-Policy", "default-src 'none'; script-src 'unsafe-inline'; style-src 'unsafe-inline'; connect-src 'self'; media-src blob:; base-uri 'none'; frame-ancestors 'none'; form-action 'none'")
	io.WriteString(w, player)
}

func main() {
	transport := http.DefaultTransport.(*http.Transport).Clone()
	transport.Proxy = nil
	mobile := &Mobile{client: &http.Client{
		Timeout:       13 * time.Second,
		Transport:     transport,
		CheckRedirect: func(*http.Request, []*http.Request) error { return http.ErrUseLastResponse },
	}}
	if err := mygo.Protocol.HandleFunc("galscaler", servePlayer); err != nil {
		log.Fatal(err)
	}
	mygo.Bind(mobile)
	mygo.App.WhenReady(func() {
		mobile.Playing(false)
		win := mygo.NewWindow(mygo.WindowOptions{
			Title:           "GalScaler",
			BackgroundColor: "#090b0f",
			Page:            mygo.PageOptions{Autoplay: mygo.AutoplayAllow, DevTools: mygo.DevToolsEnabled},
		})
		if err := win.Page().LoadURL("galscaler://localhost/"); err != nil {
			log.Print(err)
		}
	})
	if err := mygo.App.Run(); err != nil {
		log.Fatal(err)
	}
}
