@extends('layouts.app')

@section('content')
    <h1>Posts</h1>

    @if(!empty($posts))
        @foreach($posts as $post)
            <article>
                <h2>{{ $post->title }}</h2>
                @if($post->is_published)
                    <span class="badge">Published</span>
                @endif
            </article>
        @endforeach
    @else
        <p>No posts yet.</p>
    @endif
@endsection
