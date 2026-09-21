; QML: tree-sitter-qmljs ships no tags query (TAGS_QUERY is commented out of
; the crate), so ragpilot supplies one in the @definition.* / @reference.call
; convention.
(function_declaration name: (identifier) @name) @definition.function
(ui_signal name: (identifier) @name) @definition.signal
(ui_property name: (identifier) @name) @definition.property
(ui_inline_component name: (identifier) @name) @definition.component

; `id: panel` is how QML objects refer to one another.
(ui_object_definition
  initializer: (ui_object_initializer
    (ui_binding
      name: (identifier) @_key
      value: (expression_statement (identifier) @name))
    (#eq? @_key "id"))) @definition.object

(call_expression function: (identifier) @name) @reference.call
(call_expression function: (member_expression property: (property_identifier) @name)) @reference.call

; A component is used by its type name: `Audio { … }` instantiates
; Audio.qml, and `Audio.volume` reaches a singleton. Recording these as edges
; is what lets "who uses Audio" and impact analysis work, since QML imports
; only name directories. Capitalised names only — lowercase ones are ids and
; properties.
((ui_object_definition type_name: (identifier) @name) @reference.call
  (#match? @name "^[A-Z]"))
((ui_object_definition_binding type_name: (identifier) @name) @reference.call
  (#match? @name "^[A-Z]"))
((member_expression object: (identifier) @name) @reference.call
  (#match? @name "^[A-Z]"))
