import test from 'node:test';
import assert from 'node:assert/strict';
import {redundantGitRanges} from './git-cache-cleanup.js';
test('cleanup needs a full baseline, honors preservation and protects unresolved history',()=>{
 const old={bases:[],tips:['a']},delta={bases:['a'],tips:['b']},baseline={bases:[],tips:['c']},conflict={bases:[],tips:['fork']};
 assert.deepEqual(redundantGitRanges([old,delta],'c',['a','b','c'],false),[]);
 assert.deepEqual(redundantGitRanges([old,delta,baseline,conflict],'c',['a','b','c'],true),[]);
 assert.deepEqual(redundantGitRanges([old,delta,baseline,conflict],'c',['a','b','c'],false),[old,delta]);
 assert.deepEqual(redundantGitRanges([baseline,conflict],'c',['c'],false),[]);
});
