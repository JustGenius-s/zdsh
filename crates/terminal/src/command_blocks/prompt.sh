__zdsh_emit() {
    local LC_ALL=C __zdsh_payload="$1" __zdsh_character __zdsh_byte __zdsh_hex=''
    while [ -n "$__zdsh_payload" ]; do
        __zdsh_character="${__zdsh_payload%"${__zdsh_payload#?}"}"
        __zdsh_payload="${__zdsh_payload#?}"
        builtin printf -v __zdsh_byte '%d' "'$__zdsh_character"
        builtin printf -v __zdsh_byte '%02x' "$((__zdsh_byte & 255))"
        __zdsh_hex="$__zdsh_hex$__zdsh_byte"
    done
    builtin printf '\033Pd%s\033\\' "$__zdsh_hex"
}
__zdsh_json() {
    local LC_ALL=C __zdsh_value="$1" __zdsh_character __zdsh_code
    __zdsh_json_result='"'
    while [ -n "$__zdsh_value" ]; do
        __zdsh_character="${__zdsh_value%"${__zdsh_value#?}"}"
        __zdsh_value="${__zdsh_value#?}"
        case "$__zdsh_character" in
            '"'|'\') __zdsh_json_result="$__zdsh_json_result\$__zdsh_character" ;;
            [[:cntrl:]])
                builtin printf -v __zdsh_code '%d' "'$__zdsh_character"
                builtin printf -v __zdsh_character '\\u%04x' "$__zdsh_code"
                __zdsh_json_result="$__zdsh_json_result$__zdsh_character" ;;
            *) __zdsh_json_result="$__zdsh_json_result$__zdsh_character" ;;
        esac
    done
    __zdsh_json_result="$__zdsh_json_result\""
}
