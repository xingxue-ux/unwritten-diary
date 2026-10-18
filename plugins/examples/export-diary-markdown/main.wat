(module
  ;; ABI v1：宿主只挂 "unwritten" 命名空间；没有 WASI / 文件系统 / 网络。
  (import "unwritten" "selected_diary_count" (func $count (result i32)))
  (import "unwritten" "selected_diary_day" (func $day (param i32 i32 i32) (result i32)))
  (import "unwritten" "selected_diary_body" (func $body (param i32 i32 i32) (result i32)))
  (import "unwritten" "export_write" (func $export (param i32 i32 i32 i32 i32 i32) (result i32)))

  ;; 16 页 = 1 MiB；正文缓冲区 64 KiB，输出区从 128 KiB 开始。
  (memory (export "memory") 16)

  (data (i32.const 0) "## ")
  (data (i32.const 8) "\n\n")
  (data (i32.const 16) "diary.md")
  (data (i32.const 32) "text/markdown")

  (func (export "abi_version") (result i32) i32.const 1)

  ;; 读取用户选定的日记版本，按 dayKey 生成 Markdown。
  ;; 返回 0 成功；2 没选日记；3/4 读宿主内存失败；5 写导出物失败。
  (func (export "run") (result i32)
    (local $count i32)
    (local $i i32)
    (local $len i32)
    (local $out i32)

    call $count
    local.tee $count
    i32.eqz
    if
      i32.const 2
      return
    end

    i32.const 131072
    local.set $out
    i32.const 0
    local.set $i

    block $done
      loop $loop
        local.get $i
        local.get $count
        i32.ge_s
        br_if $done

        ;; dayKey
        local.get $i
        i32.const 1024
        i32.const 128
        call $day
        local.tee $len
        i32.const 0
        i32.lt_s
        if
          i32.const 3
          return
        end

        ;; "## " + dayKey + "\n\n"
        local.get $out
        i32.const 0
        i32.const 3
        memory.copy
        local.get $out
        i32.const 3
        i32.add
        local.set $out

        local.get $out
        i32.const 1024
        local.get $len
        memory.copy
        local.get $out
        local.get $len
        i32.add
        local.set $out

        local.get $out
        i32.const 8
        i32.const 2
        memory.copy
        local.get $out
        i32.const 2
        i32.add
        local.set $out

        ;; body + "\n\n"
        local.get $i
        i32.const 4096
        i32.const 65536
        call $body
        local.tee $len
        i32.const 0
        i32.lt_s
        if
          i32.const 4
          return
        end

        local.get $out
        i32.const 4096
        local.get $len
        memory.copy
        local.get $out
        local.get $len
        i32.add
        local.set $out

        local.get $out
        i32.const 8
        i32.const 2
        memory.copy
        local.get $out
        i32.const 2
        i32.add
        local.set $out

        local.get $i
        i32.const 1
        i32.add
        local.set $i
        br $loop
      end
    end

    ;; export_write("diary.md", "text/markdown", output)
    i32.const 16
    i32.const 8
    i32.const 32
    i32.const 13
    i32.const 131072
    local.get $out
    i32.const 131072
    i32.sub
    call $export
    i32.const 0
    i32.lt_s
    if
      i32.const 5
      return
    end
    i32.const 0
  )
)
