; GDScript: tree-sitter-gdscript ships no tags query (it is commented out of the
; crate and its queries/ directory is not published), so ragpilot supplies one
; in the standard @definition.* / @reference.call convention.
(function_definition name: (name) @name) @definition.function
(lambda name: (name) @name) @definition.function
(class_definition name: (name) @name) @definition.class
(class_name_statement name: (name) @name) @definition.class
(signal_statement name: (name) @name) @definition.signal
(enum_definition name: (name) @name) @definition.enum
(const_statement name: (name) @name) @definition.constant
(call (identifier) @name) @reference.call
(attribute_call (identifier) @name) @reference.call
(base_call (identifier) @name) @reference.call
