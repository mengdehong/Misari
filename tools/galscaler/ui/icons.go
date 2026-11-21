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
	return buttonIcon(c, ui.PrimaryButton(c, ""), code, label).Width(48)
}

func iconButton(c *ui.Context, code icon, label string) ui.Element {
	b := ui.ButtonBase(c).TextColor(muted)
	if b.Pressed() {
		b.Background(accent.Alpha(.14))
	} else if b.Hovered() {
		b.Background(accent.Alpha(.08))
	}
	return buttonIcon(c, b, code, label)
}

func buttonIcon(c *ui.Context, b ui.Element, code icon, label string) ui.Element {
	return pressButton(b).Size(40, 40).Padding(0).Radius(10).Label(label).Tooltip(label).FocusRing(true).
		Children(func() {
			glyph(c, code).FontSize(19)
		})
}

func actionButton(c *ui.Context, code icon, label string, primary bool) ui.Element {
	button := ui.Button
	if primary {
		button = ui.PrimaryButton
	}
	return pressButton(button(c, "")).Height(42).Padding(0, 15).Gap(8).Label(label).FocusRing(true).
		Children(func() {
			glyph(c, code).FontSize(18)
			ui.Text(c, label).FontWeight(600).NoWrap()
		})
}

func pressButton(b ui.Element) ui.Element {
	if b.Pressed() {
		b.InsetShadow(0, 2, 4, 0, heading.Alpha(.22))
	}
	return b.Transition(ui.ElementTransition{Colors: true, Shadows: true, Duration: 100 * time.Millisecond})
}
