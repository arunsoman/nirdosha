define i64 @main() {
entry:
  br label %loop

loop:
  %i = phi i64 [ 0, %entry ], [ %i.next, %loop ]
  %acc = phi i64 [ 0, %entry ], [ %acc.next, %loop ]
  %mul = mul i64 %i, 2654435761
  %val = add i64 %mul, 1
  %acc.next = xor i64 %acc, %val
  %i.next = add i64 %i, 1
  %cond = icmp slt i64 %i.next, 500000000
  br i1 %cond, label %loop, label %done

done:
  ret i64 %acc.next
}
