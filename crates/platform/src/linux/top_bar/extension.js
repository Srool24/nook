// Nook in the top bar.
//
// GNOME Shell draws the top bar above every app window, so Nook's notch,
// placed at the top of the screen, would sit under it. This lifts Nook's
// panel window (WM_CLASS "nook", title "Nook") into the group Mutter keeps
// for windows that float above the Shell's chrome. Nothing else changes:
// the window stays managed (focus, workspaces), and turning the extension
// off puts it back.

import GLib from 'gi://GLib';
import {Extension} from 'resource:///org/gnome/shell/extensions/extension.js';

const isNook = (w) =>
    w && (w.get_wm_class() ?? '').toLowerCase() === 'nook' && w.get_title() === 'Nook';

export default class NookTopBar extends Extension {
    enable() {
        this._lifted = new Set();
        this._signals = [
            global.display.connect('window-created', (_d, w) => {
                // The actor and the title can come a moment after.
                GLib.idle_add(GLib.PRIORITY_DEFAULT, () => {
                    this._lift(w);
                    return GLib.SOURCE_REMOVE;
                });
            }),
            global.display.connect('restacked', () => this._liftAll()),
        ];
        this._liftAll();
    }

    disable() {
        for (const id of this._signals)
            global.display.disconnect(id);
        this._signals = [];
        for (const actor of this._lifted) {
            if (actor.get_parent() === global.top_window_group) {
                global.top_window_group.remove_child(actor);
                global.window_group.add_child(actor);
            }
        }
        this._lifted = null;
    }

    _liftAll() {
        for (const actor of global.get_window_actors())
            this._lift(actor.meta_window);
    }

    _lift(w) {
        if (!this._lifted || !isNook(w))
            return;
        const actor = w.get_compositor_private();
        if (!actor || actor.get_parent() === global.top_window_group)
            return;
        actor.get_parent()?.remove_child(actor);
        global.top_window_group.add_child(actor);
        // Mutter may have kept the window below the bar; Nook asked for
        // the top of the monitor.
        const frame = w.get_frame_rect();
        const monitor = global.display.get_monitor_geometry(w.get_monitor());
        if (frame.y > monitor.y)
            w.move_frame(true, frame.x, monitor.y);
        if (!this._lifted.has(actor)) {
            this._lifted.add(actor);
            actor.connect('destroy', () => this._lifted?.delete(actor));
        }
    }
}
