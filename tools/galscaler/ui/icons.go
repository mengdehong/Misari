package main

import (
	_ "embed"
	"time"

	"github.com/egoist/mygo/ui"
)

//go:embed assets/lucide.ttf
var iconFont []byte

const iconFamily = "lucide"

type icon string

const (
	iGame         icon = "\ue0e2" // gamepad-2
	iLibrary      icon = "\ue105" // library
	iGear         icon = "\ue157" // settings
	iSearch       icon = "\ue154" // search
	iAdd          icon = "\ue140" // plus
	iPlay         icon = "\ue13f" // play
	iStop         icon = "\ue16a" // square
	iClose        icon = "\ue1b1" // x
	iCheck        icon = "\ue070" // check
	iFolder       icon = "\ue246" // folder-open
	iScan         icon = "\ue33c" // folder-search
	iTrash        icon = "\ue18d" // trash-2
	iLog          icon = "\ue0d0" // file-text
	iCopy         icon = "\ue0a2" // copy
	iDown         icon = "\ue071" // chevron-down
	iPrev         icon = "\ue072" // chevron-left
	iNext         icon = "\ue073" // chevron-right
	iSliders      icon = "\ue299" // sliders-horizontal
	iWarning      icon = "\ue192" // triangle-alert
	iLoading      icon = "\ue10d" // loader-circle
	iImage        icon = "\ue0f9" // image
	iDisconnected icon = "\ue461" // unplug
	iNetwork      icon = "\ue128" // network
	iRefresh      icon = "\ue148" // refresh-cw
	iMore         icon = "\ue0ba" // ellipsis
	iGrid         icon = "\ue104" // layout-grid
	iList         icon = "\ue10b" // list
	iMinus        icon = "\ue11f" // minus
	iRestore      icon = "\ue0a2" // copy
)

func glyph(c *ui.Context, code icon) ui.Element {
	return ui.Text(c, string(code)).Font(iconFamily).FontSize(20).LineHeight(1).AccessHidden(true)
}

func primaryIconButton(c *ui.Context, code icon, label string) ui.Element {
	return buttonIcon(c, primaryButton(c, ""), code, label).Width(48)
}

func iconButton(c *ui.Context, code icon, label string) ui.Element {
	p := skin(c)
	b := ui.ButtonBase(c).TextColor(p.muted)
	if b.Pressed() {
		b.Background(p.accent.Alpha(.14))
	} else if b.Hovered() {
		b.Background(p.accent.Alpha(.08))
	}
	return buttonIcon(c, b, code, label)
}

func buttonIcon(c *ui.Context, b ui.Element, code icon, label string) ui.Element {
	offset := buttonPress(c, b)
	return b.Size(40, 40).Padding(0).Radius(controlRadius).Label(label).Tooltip(label).FocusRing(true).
		Children(func() {
			glyph(c, code).FontSize(19).Translate(0, offset)
		})
}

func actionButton(c *ui.Context, code icon, label string, primary bool) ui.Element {
	button := ui.Button
	if primary {
		button = primaryButton
	}
	b := button(c, "")
	offset := buttonPress(c, b)
	return b.Height(42).Padding(0, 15).Gap(8).Label(label).FocusRing(true).
		Children(func() {
			glyph(c, code).FontSize(18).Translate(0, offset)
			ui.Text(c, label).FontWeight(600).NoWrap().Translate(0, offset)
		})
}

func primaryButton(c *ui.Context, label string) ui.Element {
	p := skin(c)
	b := ui.PrimaryButton(c, label)
	from, to := p.actionFrom, p.actionTo
	if b.Pressed() {
		from, to = from.Mix(p.shadow, .18), to.Mix(p.shadow, .18)
	} else if b.Hovered() {
		from, to = from.Mix(p.panel, .035), to.Mix(p.panel, .035)
	}
	return b.Gradient(from, to, 115).Border(0, ui.Transparent).TextColor(ui.Hex("#ffffff"))
}

func quietButton(c *ui.Context, b ui.Element) ui.Element {
	p := skin(c)
	fill, text := ui.Transparent, p.muted
	if b.Hovered() || b.Pressed() {
		fill, text = p.raised, p.heading
	}
	return b.Background(fill).Border(0, ui.Transparent).TextColor(text)
}

func pressButton(c *ui.Context, b ui.Element) ui.Element {
	buttonPress(c, b)
	return b
}

func buttonPress(c *ui.Context, b ui.Element) float32 {
	depth := float32(0)
	if b.Pressed() {
		depth = 1
	}
	if !reducedMotion(c) {
		depth = b.AnimateTo("press", depth, quickSpring)
	}
	if depth > 0 {
		b.InsetShadow(0, 2*depth, 4, 0, skin(c).shadow.Alpha(.22*depth))
	}
	b.Transition(ui.ElementTransition{Colors: true, Duration: 100 * time.Millisecond})
	return 2 * depth
}
