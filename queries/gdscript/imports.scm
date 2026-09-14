; Scripts and scenes reach each other by resource path. Only res:// paths are
; code: user:// is runtime data, and a bare load() of anything else is not a
; dependency between project files.
(call (identifier) @_fn
  arguments: (arguments . (string) @module)
  (#any-of? @_fn "preload" "load")
  (#match? @module "^.res://"))
(extends_statement (string) @module
  (#match? @module "^.res://"))
