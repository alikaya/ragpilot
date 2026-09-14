; require("mod"), require 'mod', require [[mod]] — every form parses to a call
; whose single argument is a string. Dotted paths ("a.b.c") are Lua's module
; separator, so the leaf is the last segment.
(function_call
  name: (identifier) @_fn
  arguments: (arguments (string content: (string_content) @module))
  (#eq? @_fn "require"))
