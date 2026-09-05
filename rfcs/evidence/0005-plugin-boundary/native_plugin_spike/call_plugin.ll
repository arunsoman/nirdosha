declare i64 @plugin_scale(i64)

define i64 @main() {
entry:
  br label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %i.next, %loop ]
  %acc = phi i64 [ 0, %entry ], [ %acc.next, %loop ]
  %call = call i64 @plugin_scale(i64 %i)
  %acc.next = xor i64 %acc, %call
  %i.next = add i64 %i, 1
  %cond = icmp slt i64 %i.next, 500000000
  br i1 %cond, label %loop, label %done

done:
  ret i64 %acc.next
}
