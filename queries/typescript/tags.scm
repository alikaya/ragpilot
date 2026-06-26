; TypeScript: the grammar's bundled tags.scm is signature/.d.ts-oriented and
; misses ordinary class/function/method declarations, so ragpilot ships its own
; query in the standard @definition.* / @reference.call convention.
(function_declaration name: (identifier) @name) @definition.function
(class_declaration name: (type_identifier) @name) @definition.class
(abstract_class_declaration name: (type_identifier) @name) @definition.class
(method_definition name: (property_identifier) @name) @definition.method
(interface_declaration name: (type_identifier) @name) @definition.interface
(type_alias_declaration name: (type_identifier) @name) @definition.type
(enum_declaration name: (identifier) @name) @definition.enum
(variable_declarator name: (identifier) @name value: (arrow_function)) @definition.function
(call_expression function: (identifier) @name) @reference.call
(call_expression function: (member_expression property: (property_identifier) @name)) @reference.call
