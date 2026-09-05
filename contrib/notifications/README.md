# contrib/notifications

This file states the fixed contract for `io.saola.Notifications1`. This
interface is frozen. Ask Jordan before you change a name or a value
here. The frozen-contract part of README.md must match this file at all
times.

## Session bus

The daemon controls this interface on the session bus, at these fixed
values:

- Bus name: `io.saola.Notifications1`
- Object path: `/io/saola/Notifications1`
- Interface name: `io.saola.Notifications1`

## The bus schema

**Methods**

- `ToggleCentre()` — Open the notification centre when it is closed.
  Close the notification centre when it is open.
- `OpenCentre()` — Open the notification centre. Do nothing if the
  notification centre is open.
- `CloseCentre()` — Close the notification centre. Do nothing if the
  notification centre is closed.
- `SetDnd(b)` — Set manual do-not-disturb to the given boolean value.
  This method does not set auto-DND. Auto-DND turns on with no command
  from an operator, while saola-capture records the screen.
- `Dismiss(u)` — Remove one notification, named by the given id, from
  the toast stack and from history. An unknown id gives no error. This
  method does not act on an unknown id.
- `DismissAll()` — Remove all notifications from the toast stack and
  from history.

Each removal through `Dismiss` or `DismissAll` sends one
`NotificationClosed` signal from `org.freedesktop.Notifications`, with
the values `(id, 2)`, for that removed notification. The value `2`
shows that an operator asked for the removal by hand.

**Properties**

- `NotificationCount: u` — The count of notifications in history at
  this time. History holds at most `history-cap` notifications (see
  `notifications.toml` in README.md). This count does not go above
  `history-cap`.
- `DndActive: b` — This value is `true` when do-not-disturb applies at
  this time, from `DndManual`, or from a saola-capture recording under
  way.
- `DndManual: b` — This value is `true` when an operator turns on
  do-not-disturb, through `SetDnd`, or through the toggle in the
  notification centre.
- `CentreOpen: b` — This value is `true` when the notification centre
  is open at this time.

Each property here sends the standard
`org.freedesktop.DBus.Properties.PropertiesChanged` signal each time its
value changes. The new value goes into the `changed_properties` map of
that signal. The new value does not go into the `invalidated_properties`
set of that signal. This interface has no signal of its own.

## Instructions for consumers

This part is for the indicator module of saola-panel.

- At startup, and each time this bus name gets a new owner, send the
  `org.freedesktop.DBus.Properties.GetAll` command to
  `io.saola.Notifications1`. This gives all four property values at one
  time.
- Look for the `NameOwnerChanged` signal of `org.freedesktop.DBus`, for
  the bus name `io.saola.Notifications1`. Show nothing on the indicator
  while no process holds that name.
- The daemon owns the notification centre and the toast stack. The
  panel must not build a centre popover of its own.
- A left click on the indicator sends the `ToggleCentre()` command.
- A click on the do-not-disturb toggle sends the `SetDnd(!DndManual)`
  command — the boolean value that is not the current value of
  `DndManual`. Show `DndActive`, not `DndManual`, on the glyph of the
  indicator. `DndActive` is the value that also shows a recording in
  progress under auto-DND.
- This daemon claims its bus name with `DoNotQueue` only, not with
  `ReplaceExisting`. A different notification daemon, such as mako or
  dunst, may hold `org.freedesktop.Notifications` first. When that
  happens, this interface still controls its own bus name as usual —
  the two bus names have no link. A second copy of this daemon stops at
  once, with a normal exit code, in place of an attempt to take this
  bus name from the first copy.
- The centre and the toasts sit 72 px below the strip that the panel
  keeps for itself. That strip is 84 px tall in ledger style and 76 px tall in
  Islands style, per the unit tests of saola-panel as of 2026-09-05.

## Planned (not yet served)

`HasUrgent: b` has approval for a future release. This value will be
`true` while history holds at least one critical-urgency notification
not yet seen. "Seen" will have this meaning: the notification centre
opened after that notification came in, or that notification came in
while the notification centre was open. A dismissed notification leaves
history, so it does not count. This property is not part of the interface
at this time. Consumers must not read `HasUrgent` before this file
names the release that adds it.

## Related

- saola-panel keeps its half of this link in its own
  `contrib/notifications/README.md`:
  <https://github.com/JorDunn/saola-panel/blob/main/contrib/notifications/README.md>.
  That file points at this one and gives the indicator's own drawing
  rules, its two click bindings, and its smoke test. It does not copy
  the method and property table above.
- The frozen-contract part of README.md states the same contract, in
  prose form, next to the other instructions for this daemon.
- `org.freedesktop.Notifications`, at object path
  `/org/freedesktop/Notifications`, is the standard freedesktop
  notification-daemon interface this daemon also controls. Its own two
  signals are `NotificationClosed` (values `id, reason`, both `u`) and
  `ActionInvoked` (values `id` as `u`, `action_key` as `s`). The part of
  README.md on that interface gives full instructions.
