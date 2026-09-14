; Godot shading language: the grammar crate's tags query is not exported, and
; its call pattern captures any expression; this one keeps named callees only.
(function_definition declarator: (identifier) @name) @definition.function
(struct_definition name: (identifier) @name) @definition.struct
(call_expression function: (identifier) @name) @reference.call
