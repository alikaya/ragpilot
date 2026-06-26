(function_item   name: (identifier)      @name) @function
(struct_item     name: (type_identifier) @name) @struct
(union_item      name: (type_identifier) @name) @struct
(enum_item       name: (type_identifier) @name) @enum
(trait_item      name: (type_identifier) @name) @trait
(mod_item        name: (identifier)      @name) @module
(const_item      name: (identifier)      @name) @const
(static_item     name: (identifier)      @name) @static
(type_item       name: (type_identifier) @name) @type
(macro_definition name: (identifier)     @name) @macro
(impl_item       type: (type_identifier) @name) @impl
(impl_item       type: (generic_type type: (type_identifier) @name)) @impl
